//! The 24px line under everything: what the selected server is doing, whether
//! changes are being saved, and the way into the command palette.

use gpui_kit::component::h_flex;
use gpui_kit::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, TestSupportExt as _, div, px,
};

use crate::theme::tokens;
use crate::views::{Workspace, kbd};

/// Render the status bar.
pub fn render(ws: &Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let t = *tokens(cx);
    // A confirmation takes the line for a few seconds: the status bar is
    // already where the app says what just happened.
    let copied = crate::clip::note(cx);
    let (text, unsaved) = {
        let state = ws.state.read(cx);
        // The selected server's failure is on screen when the form drawn in
        // the pane writes it, and the zoomed log does not cover the pane.
        let covered = state.drawer_open && state.drawer_zoomed;
        let failure_shown = !covered
            && state.selected_server.is_some()
            && ws
                .server_form()
                .is_some_and(|form| form.read(cx).failure_shown(cx) == state.selected_server);
        let text = match &copied {
            // A note is worded mid-sentence where it is built; the line
            // starts it with a capital.
            Some(note) => crate::views::capitalized(note),
            None => state.status_text(failure_shown),
        };
        (text, state.persistence.note())
    };
    h_flex()
        .h(px(24.))
        .flex_none()
        .px(px(12.))
        .gap(px(8.))
        .border_t_1()
        .border_color(t.hair)
        .bg(t.sunk)
        .text_size(px(11.))
        .text_color(if copied.is_some() { t.fg } else { t.muted })
        .child(
            div()
                .id("status-text")
                .aria_label(text.clone())
                .child(text)
                .test_support(),
        )
        .child(div().flex_1())
        // Stays up next to a copy confirmation: losing changes outranks it.
        .children(unsaved.map(|note| {
            div()
                .id("persistence")
                .min_w_0()
                .truncate()
                .text_color(t.err)
                .child(crate::views::capitalized(&note))
                .test_support()
        }))
        .child(
            h_flex()
                .id("commands")
                .flex_none()
                .gap(px(8.))
                .cursor_pointer()
                .hover(|s| s.text_color(t.fg))
                .on_click(cx.listener(|this, _, window, cx| this.open_palette(window, cx)))
                .child(kbd(cx, "⌘K"))
                .child("Commands")
                .test_support(),
        )
        .into_any_element()
}
