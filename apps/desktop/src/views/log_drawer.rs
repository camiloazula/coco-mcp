//! Bottom log drawer: 28px header always visible, 260px body when open, or
//! the whole space between the title bar and the status bar when zoomed.
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
use crate::theme::{Tokens, tokens};
use crate::views::json::{Fit, json_tree};
use crate::views::log_list::{LogView, fold_prefix};
use crate::views::log_menu::row_entries;
use crate::views::{Workspace, mono};

mod row;
mod search;

use row::*;
pub(crate) use search::Hits;

/// The levels the header's dropdown offers. The protocol has eight; these four are the
/// ones a person switches between while watching a server.
pub(crate) const LEVEL_CHOICES: [&str; 4] = ["debug", "info", "warning", "error"];

/// The 28px header row: chevron, "Log", count, and at the right the zoom and
/// show/hide buttons, with the filters, the level and Clear while open.
pub fn header(ws: &Workspace, window: &Window, cx: &mut Context<Workspace>) -> AnyElement {
    let t = *tokens(cx);
    let state = ws.state.read(cx);
    let open = state.drawer_open;
    let zoomed = state.drawer_zoomed;
    let active = state.log_filter;
    let level = state.features().get(Feature::LogLevel);
    // With a filter on, how many of the messages pass it.
    let (shown, total) = state.log_counts();
    let count = if state.log_is_filtered() {
        format!("{shown}/{total} messages")
    } else {
        format!("{total} messages")
    };
    let entity = ws.state.clone();
    let clear = ws.state.clone();
    let zoom = ws.state.clone();
    let toggle = ws.state.clone();
    // The icon buttons at the right, muted until hovered like the sidebar's:
    // Clear is the bin, the zoom is the arrows going out, or coming back
    // once zoomed, and the toggle is the panel with its bottom opening or
    // closing. The zoom and the toggle stay while the drawer is collapsed.
    let button = |id: &'static str, icon: gpui_kit::assets::IconName, caption: &'static str| {
        div()
            .id(id)
            .p(px(2.))
            .text_color(t.muted)
            .hover(move |s| s.text_color(t.fg))
            .cursor_pointer()
            .tooltip(move |window, cx| Tooltip::new(caption).build(window, cx))
            .child(Icon::new(icon).with_size(px(12.)))
    };
    let (toggle_icon, toggle_caption) = if open {
        (gpui_kit::assets::IconName::PanelBottomClose, "Hide logs")
    } else {
        (gpui_kit::assets::IconName::PanelBottomOpen, "Show logs")
    };
    let (zoom_icon, zoom_caption) = if zoomed {
        (gpui_kit::assets::IconName::Minimize2, "Zoom out")
    } else {
        (
            gpui_kit::assets::IconName::Maximize2,
            "Zoom the log over the columns",
        )
    };
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
    // In a window too narrow for it beside the text filter, the note gives
    // way: the dropdown's caption says the same.
    let roomy = window.viewport_size().width >= px(search::ROOMY);
    let deprecated = (level.is_deprecated() && roomy).then(|| {
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
        .child(
            mono(cx, 11., count)
                .id("log-count")
                .text_color(t.muted)
                .test_support(),
        )
        .child(div().flex_1())
        .when(open, |el| el.child(search::field(ws, cx)))
        .child(
            // One group for the filters, the level, Clear and the two
            // buttons: a click anywhere inside it, including the gaps, must
            // not reach the header toggle.
            h_flex()
                .id("log-controls")
                .h_full()
                .items_center()
                .gap(px(12.))
                .on_click(|_, _, cx| cx.stop_propagation())
                .when(open, |el| {
                    el.child(h_flex().gap(px(10.)).children(filters))
                        .children(deprecated)
                        .child(div().mr(px(12.)).child(levels))
                        .child(
                            button("clear-log", gpui_kit::assets::IconName::Trash, "Clear logs")
                                .on_click(move |_, _, cx| {
                                    cx.stop_propagation();
                                    clear.update(cx, |s, cx| s.clear_log(cx));
                                })
                                .test_support(),
                        )
                })
                .child(
                    button("zoom-log", zoom_icon, zoom_caption)
                        .on_click(move |_, _, cx| {
                            cx.stop_propagation();
                            zoom.update(cx, |s, cx| s.toggle_drawer_zoom(cx));
                        })
                        .test_support(),
                )
                .child(
                    button("toggle-log", toggle_icon, toggle_caption)
                        .on_click(move |_, _, cx| {
                            cx.stop_propagation();
                            toggle.update(cx, |s, cx| s.toggle_drawer(cx));
                        })
                        .test_support(),
                )
                .test_support(),
        )
        .test_support()
        .into_any_element()
}

/// The scrollable body (only rendered when open).
pub fn body(ws: &mut Workspace, _window: &mut Window, cx: &mut Context<Workspace>) -> AnyElement {
    let t = *tokens(cx);
    let state = ws.state.read(cx);
    let server: Option<SharedString> = state.server().map(|s| s.record.id.clone().into());
    ws.log_list.sync(LogView::of(state), ws.collapse_rev);
    if ws.log_list.is_empty() {
        let text = if server.is_some() {
            search::empty_text(state.log_filter, state.log_min_level, state.log_query())
        } else {
            "No server selected".to_owned()
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
                    let needle = state.log_needle();
                    let drawn = Drawn {
                        ix: log_ix,
                        row: r,
                        expanded: state.expanded_log.contains(&r.row_id),
                        with_agent: state.server().is_some_and(|s| s.log_has_agents()),
                        needle,
                        body: this.log_hits.body(server, needle, r),
                    };
                    row(drawn, server, this, cx)
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
