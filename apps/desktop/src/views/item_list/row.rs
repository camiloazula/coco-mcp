//! One row of the middle list.

use super::*;

/// What one row is drawn from.
pub(super) struct Row<'a> {
    pub(super) ix: usize,
    pub(super) item: &'a Item,
    pub(super) selected: Option<usize>,
    /// The row opens its detail (see `AppState::list_opens`).
    pub(super) opens: bool,
    /// The Server view's Settings row, the app's own.
    pub(super) settings: bool,
    /// Why a row that opens nothing does not.
    pub(super) why: &'static str,
}

/// One 28px row, with the accent bar when selected. A row that opens
/// nothing is drawn faint and says why on hover and when pressed, except
/// the Server view's Settings row, which is the app's own: without a
/// session it goes to the settings the pane shows, as Edit Server does.
pub(super) fn item_row(row: Row<'_>, entity: &Entity<AppState>, cx: &App) -> AnyElement {
    let Row {
        ix,
        item,
        selected,
        opens,
        settings,
        why,
    } = row;
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
        .when(!opens && settings, |el| {
            el.hover(|s| s.bg(t.hover)).on_click(|_, window, cx| {
                window.dispatch_action(Box::new(EditServer), cx);
            })
        })
        .when(!opens && !settings, |el| {
            el.opacity(0.5)
                .cursor_default()
                .tooltip(move |window, cx| Tooltip::new(why).build(window, cx))
                .on_click(move |_, _, cx| crate::clip::announce_nothing(why, cx))
        })
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
