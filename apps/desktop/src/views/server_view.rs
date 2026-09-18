//! The Server view: what a server said about itself when it connected
//! (`initialize`), which of its lists came back, and the roots offered to it.
//! Every section is drawn with nothing selected; selecting one in the list
//! shows that section alone.

use std::rc::Rc;

use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, FontWeight, InteractiveElement, IntoElement,
    ParentElement, SharedString, StatefulInteractiveElement, Styled, TestSupportExt as _, Window,
    div, px,
};
use mcp_core::{Root, Snapshot, list_method};
use serde_json::Value;

use crate::features::{DEPRECATED, Feature, Features};
use crate::state::SETTINGS_SECTION;
use crate::theme::tokens;
use crate::views::banner;
use crate::views::history::when_label;
use crate::views::json::json_tree_rc;
use crate::views::{
    Workspace, accent_button, detail_header, labelled, mono, muted, text_tab, tree_section,
};

mod roots;
mod settings;
mod snapshots;

pub(crate) use roots::parse_roots;
use roots::roots_section;
use settings::*;
use snapshots::*;

/// Render the Server view of the selected server.
pub fn render(ws: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) -> AnyElement {
    let (snapshot, section, server_id, roots, features) = {
        let state = ws.state.read(cx);
        let Some(server) = state.server() else {
            return div().into_any_element();
        };
        (
            server.snapshot().cloned(),
            state.selected_name(),
            server.record.id.clone(),
            server.roots.clone(),
            server.features(),
        )
    };
    // Settings needs no snapshot: a server that never connected has them.
    if section.as_deref() == Some(SETTINGS_SECTION) {
        return v_flex()
            .id("detail")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .pb(px(20.))
            .child(settings(ws, cx))
            .into_any_element();
    }
    let Some(snap) = snapshot else {
        return muted(cx, 13., "Nothing was captured from this server yet").into_any_element();
    };
    let wanted = |key: &str| section.as_deref().is_none_or(|s| s == key);
    let mut parts: Vec<AnyElement> = vec![header(&snap, cx)];
    if wanted("server") {
        let info = snap.server_info.clone();
        parts.push(tree(ws, "Server info", &server_id, "info", info, cx));
    }
    if wanted("capabilities") {
        let capabilities = snap.capabilities.clone();
        parts.push(tree(
            ws,
            "Capabilities",
            &server_id,
            "caps",
            capabilities,
            cx,
        ));
    }
    if let Some(text) = snap.instructions.clone().filter(|_| wanted("instructions")) {
        parts.push(instructions(text, cx));
    }
    if wanted("lists") {
        parts.push(lists(&snap, &features, cx));
    }
    if wanted("snapshots") {
        parts.push(snapshots(ws, cx));
    }
    if wanted("roots") {
        parts.push(roots_section(ws, &server_id, roots, &features, window, cx));
    }
    if wanted(SETTINGS_SECTION) {
        parts.push(settings(ws, cx));
    }
    v_flex()
        .id("detail")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .pb(px(20.))
        .children(parts)
        .into_any_element()
}

/// Name, version and negotiated protocol version.
fn header(snap: &Snapshot, cx: &Context<Workspace>) -> AnyElement {
    let t = *tokens(cx);
    let name = snap.server_name().unwrap_or("Unnamed server").to_owned();
    let title = match snap.server_version() {
        Some(version) => format!("{name} {version}"),
        None => name,
    };
    detail_header(
        mono(cx, 15., title)
            .font_weight(FontWeight::MEDIUM)
            .text_color(t.fg),
        mono(
            cx,
            11.,
            format!(
                "Protocol {} · {}",
                snap.protocol_version,
                mcp_core::Era::of(&snap.protocol_version).label()
            ),
        )
        .text_color(t.muted),
    )
    .into_any_element()
}

/// A foldable, copyable block of what `initialize` returned.
fn tree(
    ws: &Workspace,
    label: &'static str,
    server_id: &str,
    what: &str,
    value: Value,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let toggle = ws.collapse_toggle(cx);
    let prefix = format!("server:{server_id}:{what}");
    let value = Rc::new(value);
    let body = json_tree_rc(value.clone(), &ws.folds(&toggle), &prefix, cx);
    let id = SharedString::from(format!("{prefix}-copy"));
    div()
        .px(px(24.))
        .py(px(8.))
        .child(tree_section(cx, label, id, value, body))
        .into_any_element()
}

fn instructions(text: String, cx: &Context<Workspace>) -> AnyElement {
    let t = *tokens(cx);
    let body = div()
        .id("server-instructions")
        .max_w(px(640.))
        .text_size(px(13.))
        .text_color(t.fg)
        .child(text)
        .test_support()
        .into_any_element();
    div()
        .px(px(24.))
        .py(px(8.))
        .child(labelled(cx, "Instructions", div(), body))
        .into_any_element()
}

/// Each list a snapshot is taken with: how many items came back, why it did
/// not, or why the server does not offer it.
fn lists(snap: &Snapshot, features: &Features, cx: &Context<Workspace>) -> AnyElement {
    let t = *tokens(cx);
    let rows: Vec<AnyElement> = list_method::ALL
        .iter()
        .enumerate()
        .map(|(ix, method)| {
            let failure = snap.list_failures.iter().find(|f| f.method == *method);
            let not_offered = features.get(list_feature(method)).reason();
            let (text, color) = match (failure, not_offered) {
                (Some(failure), _) => (format!("Failed · {}", failure.error), t.err),
                (None, Some(reason)) => (reason.to_owned(), t.muted),
                (None, None) => (format!("{} listed", listed(snap, method)), t.fg),
            };
            h_flex()
                .id(("list-status", ix))
                .gap(px(16.))
                .child(
                    mono(cx, 12., *method)
                        .w(px(200.))
                        .flex_none()
                        .text_color(t.muted),
                )
                .child(
                    div()
                        .min_w_0()
                        .text_size(px(12.))
                        .text_color(color)
                        .child(text),
                )
                .test_support()
                .into_any_element()
        })
        .collect();
    let body = v_flex().gap(px(4.)).children(rows).into_any_element();
    div()
        .px(px(24.))
        .py(px(8.))
        .child(labelled(cx, "Lists", div(), body))
        .into_any_element()
}

/// The feature a list belongs to.
fn list_feature(method: &str) -> Feature {
    match method {
        list_method::TOOLS => Feature::Tools,
        list_method::PROMPTS => Feature::Prompts,
        _ => Feature::Resources,
    }
}

fn listed(snap: &Snapshot, method: &str) -> usize {
    match method {
        list_method::TOOLS => snap.tools.len(),
        list_method::RESOURCES => snap.resources.len(),
        list_method::RESOURCE_TEMPLATES => snap.resource_templates.len(),
        list_method::PROMPTS => snap.prompts.len(),
        _ => 0,
    }
}
