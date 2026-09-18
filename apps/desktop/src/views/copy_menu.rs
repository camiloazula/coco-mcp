//! The copy menu: a small overlay at the pointer, opened by right-clicking
//! anything that holds data.
//!
//! Drawn here rather than with gpui-component's `ContextMenu` for the reason
//! recorded on [`crate::clip::CopyMenu`], and styled like the app's other
//! overlays: hairline card, 3px radius, 12px rows.

use gpui_kit::component::v_flex;
use gpui_kit::{
    AnyElement, App, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement,
    Styled, TestSupportExt as _, div, px,
};

use crate::clip;
use crate::theme::tokens;

/// The overlay, when a menu is open.
pub fn render(cx: &App) -> Option<AnyElement> {
    let (entries, position) = clip::menu(cx)?;
    let t = *tokens(cx);
    let rows = entries.into_iter().enumerate().map(|(ix, entry)| {
        div()
            .id(("copy-menu", ix))
            .px(px(10.))
            .py(px(4.))
            .rounded(px(2.))
            .text_size(px(12.))
            .text_color(t.fg)
            .whitespace_nowrap()
            .cursor_pointer()
            .hover(|s| s.bg(t.hover))
            .on_click(move |_, _, cx| {
                clip::copy(entry.text.clone(), &entry.what, cx);
                clip::close_menu(cx);
            })
            .child(entry.label.clone())
            .test_support()
    });
    Some(
        div()
            .id("copy-menu-overlay")
            .occlude()
            .absolute()
            .inset_0()
            // Anywhere else dismisses, including a right-click that is about
            // to open the menu somewhere new.
            .on_click(|_, _, cx| clip::close_menu(cx))
            .on_mouse_down(gpui_kit::MouseButton::Right, |_, _, cx| {
                clip::close_menu(cx);
            })
            .child(
                v_flex()
                    .absolute()
                    .left(position.x)
                    .top(position.y)
                    .min_w(px(160.))
                    .p(px(4.))
                    .gap(px(1.))
                    .bg(t.bg)
                    .border_1()
                    .border_color(t.hair)
                    .rounded(px(3.))
                    .children(rows),
            )
            .into_any_element(),
    )
}
