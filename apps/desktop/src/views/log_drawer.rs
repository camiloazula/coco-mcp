//! Bottom log drawer: 28px header always visible, 260px body when open.
//! Rows: `90 | 20 | 200 | 1fr | 60`, gap 12, height 24, mono 12px.
//!
//! The rows that pass the filter are indexed once per frame; each row drawn
//! is borrowed from the model, and its copy menu is built when it opens.

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::select::Select;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, InteractiveElement, IntoElement, MouseButton, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, TestSupportExt as _, Window, div, list, px,
};

use crate::clip;
use crate::features::{Availability, DEPRECATED, Feature};
use crate::state::{Dir, LogFilter, LogRow};
use crate::theme::tokens;
use crate::views::json::json_tree;
use crate::views::log_list::{LogView, fold_prefix};
use crate::views::log_menu::row_entries;
use crate::views::{Workspace, kbd, mono};

mod row;

use row::*;

/// The levels the header's dropdown offers. The protocol has eight; these four are the
/// ones a person switches between while watching a server.
pub(crate) const LEVEL_CHOICES: [&str; 4] = ["debug", "info", "warning", "error"];

/// The 28px header row (chevron, "Log", count, Clear, ⌘J).
pub fn header(ws: &Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let t = *tokens(cx);
    let state = ws.state.read(cx);
    let open = state.drawer_open;
    let active = state.log_filter;
    let level = state.features().get(Feature::LogLevel);
    // Messages, not rows: a reconnect's separator was never on the wire.
    // With a filter on, how many of them pass it.
    let (shown, total) = state
        .server()
        .map(|s| {
            s.log()
                .iter()
                .filter(|row| !row.session_break)
                .fold((0, 0), |(shown, total), row| {
                    (shown + usize::from(state.log_row_passes(row)), total + 1)
                })
        })
        .unwrap_or((0, 0));
    let count = if active == LogFilter::All && state.log_min_level.is_none() {
        format!("{total} messages")
    } else {
        format!("{shown}/{total} messages")
    };
    let entity = ws.state.clone();
    let clear = ws.state.clone();
    // Hides log messages below the chosen level, those already shown too, and
    // asks a server that accepts it to send from that level. Rows without a
    // level (requests, stderr) always pass. The caption says which of the
    // two it does for this server.
    let caption: SharedString = match &level {
        Availability::Unavailable(reason) => {
            format!("Hides lower levels in the drawer · {reason}").into()
        }
        Availability::Deprecated(how) => format!("{DEPRECATED} · {how}").into(),
        _ => "Hides lower levels and asks the server to send from the chosen one".into(),
    };
    let levels = div()
        .id("log-level")
        .w(px(112.))
        .tooltip(move |window, cx| Tooltip::new(caption.clone()).build(window, cx))
        .child(Select::new(&ws.log_level).xsmall())
        .test_support();
    let deprecated = level.is_deprecated().then(|| {
        div()
            .id("log-level-deprecated")
            .text_size(px(11.))
            .text_color(t.muted)
            .child(DEPRECATED)
            .test_support()
    });
    let filters: Vec<AnyElement> = LogFilter::ALL
        .iter()
        .map(|&f| {
            let e = ws.state.clone();
            div()
                .id(f.label())
                .text_size(px(11.))
                .text_color(if f == active { t.fg } else { t.muted })
                .cursor_pointer()
                .on_click(move |_, _, cx| {
                    // The header's own click toggles the drawer; keep it open.
                    cx.stop_propagation();
                    e.update(cx, |s, cx| s.set_log_filter(f, cx));
                })
                .child(crate::views::capitalized(f.label()))
                .test_support()
                .into_any_element()
        })
        .collect();
    h_flex()
        .id("log-header")
        .h(px(28.))
        .flex_none()
        .px(px(12.))
        .gap(px(12.))
        .text_size(px(12.))
        .text_color(t.fg)
        .bg(t.sunk)
        .border_t_1()
        .border_color(t.hair)
        .hover(|s| s.bg(t.hover))
        .on_click(move |_, _, cx| {
            entity.update(cx, |s, cx| s.toggle_drawer(cx));
        })
        .child(
            div().w(px(10.)).text_color(t.muted).child(
                Icon::new(if open {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .with_size(px(10.))
                .text_color(t.muted),
            ),
        )
        .child("Log")
        .child(mono(cx, 11., count).text_color(t.muted))
        .child(div().flex_1())
        .when(open, |el| {
            // One group for the filters, Clear and the hint: a click anywhere
            // inside it, including the gaps, must not reach the header toggle.
            el.child(
                h_flex()
                    .id("log-controls")
                    .h_full()
                    .items_center()
                    .gap(px(12.))
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(h_flex().gap(px(10.)).children(filters))
                    .children(deprecated)
                    .child(div().mr(px(12.)).child(levels))
                    .child(
                        div()
                            .id("clear-log")
                            .text_size(px(11.))
                            .text_color(t.muted)
                            .cursor_pointer()
                            .on_click(move |_, _, cx| {
                                cx.stop_propagation();
                                clear.update(cx, |s, cx| s.clear_log(cx));
                            })
                            .child("Clear")
                            .test_support(),
                    )
                    .child(kbd(cx, "⌘J"))
                    .test_support(),
            )
        })
        .test_support()
        .into_any_element()
}

/// The scrollable body (only rendered when open).
pub fn body(ws: &mut Workspace, _window: &mut Window, cx: &mut Context<Workspace>) -> AnyElement {
    let t = *tokens(cx);
    let state = ws.state.read(cx);
    let server: Option<SharedString> = state.server().map(|s| s.record.id.clone().into());
    let filter = state.log_filter;
    let min_level = state.log_min_level;
    ws.log_list.sync(LogView::of(state), ws.collapse_rev);
    if ws.log_list.is_empty() {
        let text = match (server.is_some(), filter, min_level) {
            (false, _, _) => "No server selected".to_owned(),
            (true, LogFilter::All, None) => "No messages yet".to_owned(),
            (true, filter, None) => format!("No {} messages", filter.label()),
            (true, LogFilter::All, Some(level)) => format!("No messages at {level} or above"),
            (true, filter, Some(level)) => {
                format!("No {} messages at {level} or above", filter.label())
            }
        };
        return v_flex()
            .id("log-body")
            .size_full()
            .bg(t.sunk)
            .border_t_1()
            .border_color(t.hair)
            .items_center()
            .justify_center()
            .child(
                div()
                    .id("log-empty")
                    .text_size(px(12.))
                    .text_color(t.muted)
                    .child(text)
                    .test_support(),
            )
            .into_any_element();
    }
    let list = list(
        ws.log_list.state(),
        cx.processor(move |this, ix: usize, _window, cx| {
            let state = this.state.read(cx);
            let drawn =
                server
                    .as_ref()
                    .zip(this.log_list.row_at(ix))
                    .and_then(|(server, log_ix)| {
                        Some((server, log_ix, state.server()?.log().get(log_ix)?))
                    });
            match drawn {
                Some((server, log_ix, r)) => {
                    let expanded = state.expanded_log == Some(r.row_id);
                    row(log_ix, r, expanded, server, this, cx)
                }
                None => div().into_any_element(),
            }
        }),
    )
    .size_full();
    v_flex()
        .id("log-body")
        .size_full()
        .bg(t.sunk)
        .border_t_1()
        .border_color(t.hair)
        .font_family(cx.theme().mono_font_family.clone())
        .text_size(px(12.))
        .child(list)
        .into_any_element()
}
