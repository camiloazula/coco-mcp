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

use crate::actions::{ITEM_LIST, MoveDown, MoveUp, Open};
use crate::state::{AppState, Item, Mode};
use crate::theme::tokens;
use crate::views::list_failures::list_notice;
use crate::views::{Workspace, mono, muted};

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
    // Without a session the rows are the last connection's: shown, not opened.
    let opens = state.list_opens();
    let stale = (!opens && count > 0).then(|| {
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
            range
                .filter_map(|ix| {
                    let item = items.get(ix)?;
                    Some(item_row(ix, item, selected, opens, &this.state, cx))
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
        // keyboard into its first field, where ⌘⏎ sends.
        .on_action(cx.listener(|this, _: &Open, window, cx| {
            if this.state.read(cx).selected_item.is_none() {
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

/// One 28px row, with the accent bar when selected. A row that `opens`
/// nothing is drawn faint and takes no click.
fn item_row(
    ix: usize,
    item: &Item,
    selected: Option<usize>,
    opens: bool,
    entity: &Entity<AppState>,
    cx: &App,
) -> AnyElement {
    let t = *tokens(cx);
    let is_selected = selected == Some(ix);
    let entity = entity.clone();
    h_flex()
        .id(("item", ix))
        .relative()
        .w_full()
        .h(px(28.))
        .px(px(12.))
        .text_color(if item.failed {
            t.err
        } else if is_selected {
            t.fg
        } else {
            t.muted
        })
        .font_weight(if is_selected {
            FontWeight::MEDIUM
        } else {
            FontWeight::NORMAL
        })
        .when(is_selected, |el| el.bg(t.sel))
        .when(opens, |el| {
            el.hover(|s| s.bg(t.hover)).on_click(move |_, _, cx| {
                entity.update(cx, |state, cx| state.open_item(ix, cx));
            })
        })
        .when(!opens, |el| el.opacity(0.5).cursor_default())
        .child(
            div()
                .absolute()
                .left_0()
                .top(px(6.))
                .bottom(px(6.))
                .w(px(2.))
                .when(is_selected, |el| el.bg(t.accent)),
        )
        .gap(px(8.))
        .child(
            mono(cx, 12., item.label.clone())
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis(),
        )
        // A resource the session is subscribed to.
        .when(item.watched, |el| {
            el.child(mono(cx, 11., "Subscribed").flex_none().text_color(t.muted))
        })
        // An item the server added or altered since it was last selected.
        .when(item.changed, |el| {
            el.child(
                div()
                    .id(("changed", ix))
                    .flex_none()
                    .size(px(7.))
                    .rounded_full()
                    .bg(t.accent)
                    .test_support(),
            )
        })
        .children(
            item.meta
                .clone()
                .map(|m| mono(cx, 11., m).flex_none().text_color(t.muted)),
        )
        .test_support()
        .into_any_element()
}
