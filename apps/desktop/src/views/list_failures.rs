//! What the app says when a server advertised a list and could not deliver
//! it: the session is kept, the list is empty, and the reason is shown where
//! its items would have been.

use gpui_kit::component::v_flex;
use gpui_kit::{
    AnyElement, App, InteractiveElement as _, IntoElement, ParentElement, Styled,
    TestSupportExt as _, div, px,
};
use mcp_core::ListFailure;

use crate::theme::tokens;

/// A notice at the top of the list column, one entry per failed list.
/// `None` when every list behind the mode was read.
pub fn list_notice(failures: &[ListFailure], cx: &App) -> Option<AnyElement> {
    if failures.is_empty() {
        return None;
    }
    let t = tokens(cx);
    Some(
        v_flex()
            .id("list-failure")
            .px(px(12.))
            .py(px(6.))
            .gap(px(2.))
            .children(failures.iter().map(|f| {
                v_flex()
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(t.err)
                            .child(format!("{} failed", f.method)),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(t.err)
                            .whitespace_normal()
                            .child(f.error.clone()),
                    )
            }))
            .test_support()
            .into_any_element(),
    )
}

/// The detail pane of a mode a failed list left with no items: why there is
/// nothing to select. Same layout as the pane of a server that is not
/// connected.
pub fn detail_note(noun: &str, failures: &[ListFailure], cx: &App) -> AnyElement {
    let t = tokens(cx);
    v_flex()
        .id("list-failure-detail")
        .flex_1()
        .items_center()
        .justify_center()
        .gap(px(8.))
        .text_color(t.muted)
        .child(
            div()
                .text_size(px(15.))
                .text_color(t.fg)
                .child(format!("Could not list {noun}")),
        )
        .children(failures.iter().map(|f| {
            div()
                .text_size(px(13.))
                .text_center()
                .max_w(px(480.))
                .text_color(t.err)
                .child(format!("{}: {}", f.method, f.error))
        }))
        .test_support()
        .into_any_element()
}
