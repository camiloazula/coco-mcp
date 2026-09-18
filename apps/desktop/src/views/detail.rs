//! Right pane: empty state and the tool / resource / prompt detail with
//! the response section underneath.

use std::rc::Rc;

use gpui_kit::component::input::Input;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, Div, FontWeight, InteractiveElement, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, TestSupportExt as _, Window, div, px,
};

use crate::clip;
use crate::state::{Mode, Status};
use crate::theme::tokens;
use crate::views::content::Draw;
use crate::views::json::{Folds, json_tree, json_tree_rc};
use crate::views::list_failures::detail_note;
use crate::views::response::Shown;
use crate::views::{
    Workspace, accent_button, detail_header, history, kbd, mono, muted, response, text_tab,
    tree_section,
};

mod declared;
mod empty;
mod tool;

use declared::*;
use empty::*;
use tool::*;

/// Render the detail pane for the current selection.
pub fn render(ws: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) -> AnyElement {
    let (no_servers, server, mode) = {
        let state = ws.state.read(cx);
        (
            state.servers.is_empty(),
            state
                .server()
                .map(|s| (s.record.name.clone(), s.transport(), s.status.clone())),
            state.mode,
        )
    };
    if no_servers {
        return empty_state(ws, cx).into_any_element();
    }
    let Some((name, transport, status)) = server else {
        return div().into_any_element();
    };
    let meta = format!("{name} · {transport}");
    if mode == Mode::History {
        // History is read from the database, so it is shown even when
        // disconnected; only `Replay` needs a session.
        ws.sync_selection(window, cx);
        // The list column draws no rows until the stored calls are read, so
        // no call is shown beside it either, even one recorded meanwhile.
        if ws.state.read(cx).history_reading() {
            return centered_note(cx, "Reading history…");
        }
        return history::render(ws, cx)
            .unwrap_or_else(|| centered_note(cx, "Select a call to see it"));
    }
    match status {
        Status::Connecting => return centered_note(cx, "Connecting…"),
        Status::Error(e) => {
            let unauthorized = ws.state.read(cx).server().is_some_and(|s| s.unauthorized);
            let title = if unauthorized {
                "Authorization required"
            } else {
                "Connection failed"
            };
            return connect_state(ws, cx, title, Some(e), "Retry", unauthorized);
        }
        Status::Off => return connect_state(ws, cx, "Disconnected", None, "Connect", false),
        Status::Connected => {}
    }
    ws.sync_selection(window, cx);
    if mode == Mode::Server {
        return crate::views::server_view::render(ws, window, cx);
    }
    let selection = {
        let state = ws.state.read(cx);
        match mode {
            Mode::Tools => state.selected_tool().cloned().map(Selection::Tool),
            Mode::Resources => state
                .selected_resource()
                .cloned()
                .map(Selection::Resource)
                .or_else(|| state.selected_template().cloned().map(Selection::Template)),
            Mode::Prompts => state.selected_prompt().cloned().map(Selection::Prompt),
            Mode::History | Mode::Server => None,
        }
    };
    let body = match selection {
        Some(Selection::Tool(tool)) => tool_detail(ws, &tool, &meta, window, cx),
        Some(Selection::Resource(r)) => {
            let declared = serde_json::to_value(&r).unwrap_or_default();
            let facts = ResourceFacts {
                declared,
                title: r.title,
                uri: r.uri,
                name: r.name,
                description: r.description,
                mime: r.mime_type,
                size: r.size,
                concrete: true,
            };
            resource_detail(ws, facts, &meta, window, cx)
        }
        Some(Selection::Template(t)) => {
            let declared = serde_json::to_value(&t).unwrap_or_default();
            let facts = ResourceFacts {
                declared,
                title: t.title,
                uri: t.uri_template,
                name: t.name,
                description: t.description,
                mime: t.mime_type,
                size: None,
                concrete: false,
            };
            resource_detail(ws, facts, &meta, window, cx)
        }
        Some(Selection::Prompt(p)) => prompt_detail(ws, &p, &meta, window, cx),
        None => {
            let (failures, empty) = {
                let state = ws.state.read(cx);
                (state.list_failures(mode), state.count(mode) == Some(0))
            };
            let noun = mode.label().to_lowercase();
            // Beside a list that was read there is something to select, even
            // when the filter hides it; the list column names the failure.
            if empty && !failures.is_empty() {
                return detail_note(&noun, &failures, cx);
            }
            if empty {
                return centered_note(cx, format!("This server lists no {noun}"));
            }
            return centered_note(
                cx,
                format!("Select a {} to inspect it", noun.trim_end_matches('s')),
            );
        }
    };
    let toggle = ws.collapse_toggle(cx);
    let prefix = ws.response_prefix(cx);
    let revealed = ws.revealed.contains(&prefix);
    // Field by field, so the blob cache can be borrowed mutably beside them.
    let folds = Folds {
        collapsed: &ws.collapsed,
        unfolded: &ws.unfolded,
        toggle: &toggle,
    };
    let mut draw = Draw {
        folds: &folds,
        decoded: &mut ws.decoded,
    };
    let state = ws.state.read(cx);
    let response = state.response().map(|r| {
        let shown = Shown::live(r).waiting(state.response_waiting());
        response::render(shown, &prefix, revealed, &mut draw, cx)
    });
    v_flex()
        .id("detail")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .children(body)
        .child(div().flex_1())
        .children(response)
        .into_any_element()
}

enum Selection {
    Tool(mcp_core::Tool),
    Resource(mcp_core::Resource),
    Template(mcp_core::ResourceTemplate),
    Prompt(mcp_core::Prompt),
}

struct ResourceFacts {
    /// The resource or template as the server listed it.
    declared: serde_json::Value,
    title: Option<String>,
    uri: String,
    name: String,
    description: Option<String>,
    mime: Option<String>,
    size: Option<u64>,
    /// A concrete resource, which can be subscribed to; a template cannot.
    concrete: bool,
}

/// Suggestions shown under one argument input.
const MAX_SUGGESTIONS: usize = 8;

fn header(cx: &Context<Workspace>, title: String, meta: &str) -> Div {
    let t = *tokens(cx);
    detail_header(
        mono(cx, 15., title)
            .font_weight(FontWeight::MEDIUM)
            .text_color(t.fg),
        mono(cx, 11., meta.to_owned()).text_color(t.muted),
    )
}

fn description(cx: &Context<Workspace>, text: Option<&str>) -> Option<Div> {
    text.map(|d| {
        div()
            .px(px(24.))
            .pb(px(16.))
            .text_color(tokens(cx).muted)
            .max_w(px(640.))
            .child(d.to_owned())
    })
}

/// Which tabs a detail's toolbar offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tabs {
    /// `Form`, `Raw` and `Schema`, for a tool.
    Tool,
    /// `Detail` and `Schema`, for a resource, template or prompt.
    Declared,
}

/// Toolbar: the tabs at left, `⌘⏎` + action button at right.
fn toolbar(ws: &Workspace, cx: &mut Context<Workspace>, tabs: Tabs, action: &'static str) -> Div {
    let t = *tokens(cx);
    let schema_tab = ws.selection.schema_tab;
    let raw = !schema_tab
        && ws
            .selection
            .form
            .as_ref()
            .map(|f| f.read(cx).raw_mode)
            .unwrap_or(false);
    let pending = ws.state.read(cx).response_pending();
    h_flex()
        .h(px(36.))
        .flex_none()
        .px(px(24.))
        .justify_between()
        .border_t_1()
        .border_b_1()
        .border_color(t.hair)
        .child(
            h_flex()
                .gap(px(16.))
                .when(tabs == Tabs::Declared, |el| {
                    el.child(
                        text_tab(cx, "Detail", !schema_tab)
                            .id("tab-detail")
                            .on_click(cx.listener(|this, _, _, cx| this.set_detail_tab(cx)))
                            .test_support(),
                    )
                    .child(
                        text_tab(cx, "Schema", schema_tab)
                            .id("tab-schema")
                            .on_click(cx.listener(|this, _, _, cx| this.set_schema_tab(cx)))
                            .test_support(),
                    )
                })
                .when(tabs == Tabs::Tool, |el| {
                    el.child(
                        text_tab(cx, "Form", !raw && !schema_tab)
                            .id("tab-form")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.set_raw(false, window, cx)),
                            )
                            .test_support(),
                    )
                    .child(
                        text_tab(cx, "Raw", raw)
                            .id("tab-raw")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.set_raw(true, window, cx)),
                            )
                            .test_support(),
                    )
                    .child(
                        text_tab(cx, "Schema", schema_tab)
                            .id("tab-schema")
                            .on_click(cx.listener(|this, _, _, cx| this.set_schema_tab(cx)))
                            .test_support(),
                    )
                }),
        )
        .child(
            h_flex()
                .gap(px(10.))
                .items_center()
                // The request as it stands, before it is sent: the shape a
                // bug report or a test fixture needs.
                .child(
                    clip::icon_button(
                        cx,
                        "copy-request",
                        gpui_kit::assets::IconName::Copy,
                        "Copy the request as JSON-RPC",
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.copy_request(cx)))
                    .test_support(),
                )
                .child(kbd(cx, "⌘⏎"))
                .child(
                    accent_button(cx, action, 24.)
                        .id("call")
                        .when(pending, |el| el.opacity(0.5))
                        .on_click(cx.listener(|this, _, window, cx| this.perform_call(window, cx)))
                        .test_support(),
                ),
        )
}
