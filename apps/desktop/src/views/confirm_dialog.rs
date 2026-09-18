//! Confirmation before something that cannot be undone: deleting a server,
//! clearing its call history, forgetting its credentials. Same overlay chrome
//! as the request dialog; the destructive button uses the `err` token.

use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::{
    AnyElement, Context, FontWeight, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, TestSupportExt as _, Window, div, hsla, px,
};

use crate::actions::{DIALOG, DialogAccept};
use crate::state::Confirm;
use crate::theme::tokens;
use crate::views::{Workspace, muted};

/// The overlay, when a confirmation is pending.
pub fn render(
    ws: &mut Workspace,
    _window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Option<AnyElement> {
    let t = *tokens(cx);
    let (title, question, consequence, action) = {
        let state = ws.state.read(cx);
        let confirm = state.confirm?;
        let name = state.servers.get(confirm.server())?.record.name.clone();
        match confirm {
            Confirm::Delete(_) => (
                "Delete server",
                format!("Are you sure you want to delete the MCP server “{name}”?"),
                "Its call history, snapshots and stored secret are deleted too. This cannot be undone.",
                "Delete",
            ),
            Confirm::ClearHistory(_) => (
                "Clear history",
                format!("Delete every recorded call of “{name}”?"),
                "The server, its snapshots and its credentials stay. This cannot be undone.",
                "Clear",
            ),
            Confirm::ForgetCredentials(_) => (
                "Forget credentials",
                format!("Remove the stored credentials of “{name}”?"),
                "The server stays; it asks for a token or an authorization on its next connect.",
                "Forget",
            ),
        }
    };
    let card = v_flex()
        .id("confirm-dialog")
        .key_context(DIALOG)
        .track_focus(&ws.dialog_focus)
        .on_action(cx.listener(|this, _: &DialogAccept, _, cx| {
            this.state.update(cx, |s, cx| s.confirm(cx));
        }))
        .w(px(440.))
        .bg(t.bg)
        .border_1()
        .border_color(t.hair)
        .rounded(px(3.))
        .child(
            h_flex()
                .h(px(36.))
                .flex_none()
                .px(px(16.))
                .border_b_1()
                .border_color(t.hair)
                .child(
                    div()
                        .text_size(px(13.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(t.fg)
                        .child(title),
                ),
        )
        .child(
            v_flex()
                .px(px(16.))
                .py(px(12.))
                .gap(px(6.))
                .child(div().text_size(px(13.)).text_color(t.fg).child(question))
                .child(muted(cx, 12., consequence)),
        )
        .child(
            h_flex()
                .h(px(44.))
                .flex_none()
                .px(px(16.))
                .gap(px(16.))
                .justify_end()
                .border_t_1()
                .border_color(t.hair)
                .child(
                    div()
                        .id("confirm-cancel")
                        .text_size(px(12.))
                        .text_color(t.muted)
                        .hover(|s| s.text_color(t.fg))
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.state.update(cx, |s, cx| s.cancel_confirm(cx));
                        }))
                        .child("Cancel")
                        .test_support(),
                )
                .child(
                    h_flex()
                        .id("confirm-accept")
                        .h(px(24.))
                        .px(px(12.))
                        .rounded(px(3.))
                        .bg(t.err)
                        .text_color(t.on_accent)
                        .text_size(px(12.))
                        .font_weight(FontWeight::MEDIUM)
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.state.update(cx, |s, cx| s.confirm(cx));
                        }))
                        .child(action)
                        .test_support(),
                ),
        )
        .test_support();
    Some(
        div()
            .id("confirm-overlay")
            .occlude()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(hsla(0., 0., 0., 0.5))
            .child(card)
            .into_any_element(),
    )
}
