//! Middle column: filter row and item rows. 280px wide.
//!
//! Rows are a uniform 28px, so they are drawn by gpui's `uniform_list`: only
//! the rows in view are built, however long the list.

use std::ops::Range;

use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, Entity, FocusHandle, FontWeight, InteractiveElement, IntoElement,
    ParentElement, ScrollStrategy, StatefulInteractiveElement, Styled, TestSupportExt as _, Window,
    div, px, uniform_list,
};

use crate::actions::{EditServer, ITEM_LIST, MoveDown, MoveUp, Open};
use crate::state::{AppState, Item, Mode, SETTINGS_SECTION, Status};
use crate::theme::tokens;
use crate::views::list_failures::list_notice;
use crate::views::{Workspace, mono, muted};

mod row;

use row::*;

/// Render the list column.
pub fn render(
    ws: &Workspace,
    filter: &Entity<InputState>,
    focus: &FocusHandle,
    _window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let t = *tokens(cx);
    let state = ws.state.read(cx);
    let count = state.items().len();
    let label = state.list_count();
    let reading = state.history_reading();
    // The History list clears from where it is read, not only from the menu.
    let clearable = state.mode == Mode::History && !reading && state.items_total() > 0;
    // Above the rows rather than among them: every row is sized from the first.
    let notice = list_notice(&state.list_failures(state.mode), cx);
    // Without a session the rows are the last connection's: shown, not
    // opened. The note goes with the settings pane, whose Connect it names;
    // while connecting, the rows are only waited on.
    // The Server view's Settings row is the app's own, not something a
    // connection declared, and is left out of that.
    let declared = if state.mode == Mode::Server {
        state
            .items()
            .iter()
            .filter(|i| i.name != SETTINGS_SECTION)
            .count()
    } else {
        count
    };
    let stale = (state.settings_pane().is_some() && declared > 0).then(|| {
        muted(cx, 11., "From the last connection. Connect to open one.")
            .id("list-stale")
            .px(px(12.))
            .pt(px(8.))
            .whitespace_normal()
            .test_support()
    });

    let filter_row = h_flex()
        .h(px(36.))
        .flex_none()
        .px(px(12.))
        .gap(px(8.))
        .border_b_1()
        .border_color(t.hair)
        .text_color(t.muted)
        .child(
            Icon::new(IconName::Search)
                .with_size(px(13.))
                .text_color(t.muted),
        )
        .child(
            div().flex_1().min_w_0().child(
                Input::new(filter)
                    .id("filter")
                    .appearance(false)
                    .bordered(false)
                    .focus_bordered(false)
                    .w_full(),
            ),
        )
        .child(mono(cx, 11., label))
        .when(clearable, |el| {
            el.child(
                div()
                    .id("clear-history")
                    .p(px(2.))
                    .text_color(t.muted)
                    .hover(|s| s.text_color(t.err))
                    .cursor_pointer()
                    .tooltip(|window, cx| {
                        Tooltip::new("Clear history… (asks first)").build(window, cx)
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.state.update(cx, |s, cx| s.request_clear_history(cx));
                    }))
                    .child(Icon::new(gpui_kit::assets::IconName::Trash).with_size(px(12.)))
                    .test_support(),
            )
        });

    let rows = uniform_list(
        "item-rows",
        count,
        cx.processor(|this, range: Range<usize>, _window, cx| {
            let state = this.state.read(cx);
            let items = state.items();
            let opens = state.list_opens();
            let selected = state.selected_item.filter(|_| opens);
            let server_view = state.mode == Mode::Server;
            // What a faint row says when hovered or pressed.
            let why = if state.server().map(|s| &s.status) == Some(&Status::Connecting) {
                "The server is connecting"
            } else {
                "Connect the server to open it"
            };
            range
                .filter_map(|ix| {
                    let item = items.get(ix)?;
                    let settings = server_view && item.name == SETTINGS_SECTION;
                    let row = Row {
                        ix,
                        item,
                        selected,
                        opens,
                        settings,
                        why,
                    };
                    Some(item_row(row, &this.state, cx))
                })
                .collect::<Vec<_>>()
        }),
    )
    .track_scroll(&ws.item_scroll)
    .w_full()
    .flex_1()
    .min_h_0()
    .py(px(6.))
    .test_support();
    let body = if reading {
        reading_note(cx)
    } else if count == 0 {
        let text = if !state.filter.is_empty() && state.items_total() > 0 {
            "Nothing matches the filter".to_owned()
        } else if state.mode == crate::state::Mode::History {
            "No calls recorded yet".to_owned()
        } else {
            format!("No {}", state.mode.label().to_lowercase())
        };
        note_row(cx, "list-empty", text)
    } else {
        rows.into_any_element()
    };

    v_flex()
        .id("item-list")
        .key_context(ITEM_LIST)
        .track_focus(focus)
        .on_action(cx.listener(|this, _: &MoveUp, _, cx| {
            this.state.update(cx, |s, cx| s.move_item(-1, cx));
            reveal_selection(this, cx);
        }))
        .on_action(cx.listener(|this, _: &MoveDown, _, cx| {
            this.state.update(cx, |s, cx| s.move_item(1, cx));
            reveal_selection(this, cx);
        }))
        // Enter selects the first row, or on a selected one moves the
        // keyboard into its first field, where ⌘⏎ sends. Rows that open
        // nothing leave the pane its settings: Enter goes there instead.
        .on_action(cx.listener(|this, _: &Open, window, cx| {
            if !this.state.read(cx).list_opens() {
                this.focus_settings_pane(window, cx);
            } else if this.state.read(cx).selected_item.is_none() {
                this.state.update(cx, |s, cx| s.move_item(1, cx));
                reveal_selection(this, cx);
            } else {
                this.focus_detail(window, cx);
            }
        }))
        .h_full()
        .w_full()
        .child(filter_row)
        .children(notice)
        .children(stale)
        .child(body)
        .into_any_element()
}

/// Stands in for the rows while the selected server's stored calls are read.
fn reading_note(cx: &App) -> AnyElement {
    note_row(cx, "history-loading", "Reading history…")
}

/// One muted 28px line where the rows would be.
fn note_row(cx: &App, id: &'static str, text: impl Into<gpui_kit::SharedString>) -> AnyElement {
    v_flex()
        .flex_1()
        .min_h_0()
        .py(px(6.))
        .child(
            muted(cx, 12., text)
                .id(id)
                .flex()
                .items_center()
                .h(px(28.))
                .px(px(12.))
                .test_support(),
        )
        .into_any_element()
}

/// Scroll the selected row into view when a key or a selection by name moved
/// it off screen; a row already in view stays where it is.
pub(crate) fn reveal_selection(ws: &Workspace, cx: &App) {
    if let Some(ix) = ws.state.read(cx).selected_item {
        ws.item_scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
    }
}
