//! Modal for server-initiated requests: `elicitation/create`,
//! `sampling/createMessage` and `roots/list` (the answers are built in
//! `request_answers.rs`). Hand-written overlay rather than
//! gpui-component's `Dialog`: the design's dialog chrome (hairline card,
//! text buttons, accent primary) does not match the component's styling,
//! and the overlay is a few dozen lines.

use std::rc::Rc;

use gpui_kit::component::input::{Input, Textarea};
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, FontWeight, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, TestSupportExt as _, Window, div, hsla, px,
};
use mcp_core::{ElicitationMode, ServerRequestKind};
use serde_json::Value;

use crate::actions::{DIALOG, DialogAccept};
use crate::features::{DEPRECATED, Feature};
use crate::theme::tokens;
use crate::views::json::{Fit, json_tree, json_tree_rc};
use crate::views::{Workspace, accent_button, mono, muted, tree_section};

mod kinds;

use kinds::*;

/// Tree prefix of the dialog's request payload. One dialog is open at a time,
/// so a single key is enough.
pub const REQUEST_TREE: &str = "req:request";

/// The overlay, when a request is waiting.
pub fn render(
    ws: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Option<AnyElement> {
    ws.sync_request_ui(window, cx);
    let t = *tokens(cx);
    let (server_name, kind, queued, deprecated) = {
        let state = ws.state.read(cx);
        let head = state.current_request()?;
        // Sampling and roots still work on a 2026-07-28 server, which asks
        // for them inside a request, but the revision deprecates both.
        let feature = match &head.request.kind {
            ServerRequestKind::Sampling(_) => Some(Feature::Sampling),
            ServerRequestKind::ListRoots => Some(Feature::Roots),
            ServerRequestKind::Elicitation { .. } => None,
        };
        let deprecated = feature.zip(state.servers.iter().find(|s| s.record.id == head.server_id));
        (
            head.server_name.clone(),
            head.request.kind.clone(),
            state.pending.len() - 1,
            deprecated
                .is_some_and(|(feature, server)| server.features().get(feature).is_deprecated()),
        )
    };
    let (title, primary, secondary) = match &kind {
        ServerRequestKind::Elicitation {
            mode: ElicitationMode::Form { .. },
            ..
        } => ("Elicitation request", "Accept", Some("Decline")),
        ServerRequestKind::Elicitation { .. } => {
            ("Elicitation request", "Open in browser", Some("Decline"))
        }
        ServerRequestKind::Sampling(_) => ("Sampling request", "Reply", Some("Reject")),
        ServerRequestKind::ListRoots => ("Roots request", "Send", Some("Send none")),
    };
    let body = body(ws, &kind, window, cx);
    let meta = if queued > 0 {
        format!("{server_name} · {} · +{queued} queued", kind.method())
    } else {
        format!("{server_name} · {}", kind.method())
    };
    let text_button = |id: &'static str, label: &'static str| {
        div()
            .id(id)
            .text_size(px(12.))
            .text_color(t.muted)
            .hover(|s| s.text_color(t.fg))
            .cursor_pointer()
            .child(label)
    };
    let card = v_flex()
        .id("request-dialog")
        .key_context(DIALOG)
        .track_focus(&ws.dialog_focus)
        .on_action(cx.listener(|this, _: &DialogAccept, _, cx| this.accept_request(cx)))
        .w(px(520.))
        .max_h(px(600.))
        .bg(t.bg)
        .border_1()
        .border_color(t.hair)
        .rounded(px(3.))
        .child(
            h_flex()
                .h(px(36.))
                .flex_none()
                .px(px(16.))
                .justify_between()
                .border_b_1()
                .border_color(t.hair)
                .child(
                    div()
                        .text_size(px(13.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(t.fg)
                        .child(title),
                )
                .child(mono(cx, 11., meta).text_color(t.muted)),
        )
        .child(
            v_flex()
                .id("request-body")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .px(px(16.))
                .py(px(12.))
                .gap(px(10.))
                .children(deprecated.then(|| {
                    muted(cx, 11., DEPRECATED)
                        .id("request-deprecated")
                        .test_support()
                }))
                .child(body),
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
                    text_button("req-cancel", "Cancel")
                        .on_click(cx.listener(|this, _, _, cx| this.cancel_request(cx)))
                        .test_support(),
                )
                .children(secondary.map(|label| {
                    text_button("req-decline", label)
                        .on_click(cx.listener(|this, _, _, cx| this.decline_request(cx)))
                        .test_support()
                }))
                .child(
                    accent_button(cx, primary, 24.)
                        .id("req-accept")
                        .on_click(cx.listener(|this, _, _, cx| this.accept_request(cx)))
                        .test_support(),
                ),
        )
        .test_support();
    Some(
        div()
            .id("request-overlay")
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

/// The kind-specific body, followed by the whole request as a folded tree so
/// nothing the server sent is hidden from the person answering it.
fn body(
    ws: &mut Workspace,
    kind: &ServerRequestKind,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let detail = kind_body(ws, kind, window, cx);
    let payload = Rc::new(crate::state::server_request_payload(kind));
    let toggle = ws.collapse_toggle(cx);
    let tree = json_tree_rc(
        payload.clone(),
        &ws.folds(&toggle),
        REQUEST_TREE,
        Fit::Rows,
        cx,
    );
    v_flex()
        .gap(px(10.))
        .child(detail)
        .child(tree_section(cx, "Request", "req-copy", payload, tree))
        .into_any_element()
}
