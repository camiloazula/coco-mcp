//! Detail pane for a history row: what was sent, the stored result, and
//! `Replay` / `Open in form`.

use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, FontWeight, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, TestSupportExt as _, div, px,
};
use mcp_store::{CallKind, CallStatus};

use crate::state::Status;
use crate::theme::tokens;
use crate::views::content::Draw;
use crate::views::json::{Folds, json_tree_rc};
use crate::views::response::Shown;
use crate::views::{
    Workspace, accent_button, detail_header, kbd, mono, muted, response, tree_section,
};

/// JSON-RPC method of a call kind.
pub fn method(kind: CallKind) -> &'static str {
    match kind {
        CallKind::Tool => "tools/call",
        CallKind::Resource => "resources/read",
        CallKind::Prompt => "prompts/get",
    }
}

pub(crate) fn when_label(at: time::OffsetDateTime) -> String {
    let local = time::UtcOffset::current_local_offset()
        .map(|o| at.to_offset(o))
        .unwrap_or(at);
    let format = time::macros::format_description!("[year]-[month]-[day] [hour]:[minute]:[second]");
    local.format(&format).unwrap_or_default()
}

/// Render the pane for the selected call, or `None` when none is selected.
///
/// The record and its result are borrowed from the model, not copied, for
/// as long as the row stays selected.
pub fn render(ws: &mut Workspace, cx: &Context<Workspace>) -> Option<AnyElement> {
    let t = *tokens(cx);
    // Both trees are keyed by the call id, so a replay keeps the folds.
    let toggle = ws.collapse_toggle(cx);
    let state = ws.state.read(cx);
    let record = state.selected_call()?;
    let connected = state
        .server()
        .is_some_and(|s| s.status == Status::Connected);
    let pending = state.response_pending();
    let status = match record.status {
        CallStatus::Ok => "OK",
        CallStatus::ToolError => "isError",
        CallStatus::Failed => "Failed",
    };
    let elapsed = if record.elapsed_ms == 0 {
        "<1 ms".to_owned()
    } else {
        format!("{} ms", record.elapsed_ms)
    };
    let facts = format!(
        "{} · {status} · {elapsed} · {}",
        method(record.kind),
        when_label(record.at)
    );
    let header = detail_header(
        mono(cx, 15., record.name.clone())
            .font_weight(FontWeight::MEDIUM)
            .text_color(t.fg),
        mono(cx, 11., facts).text_color(t.muted),
    );

    let open_in_form = matches!(record.kind, CallKind::Tool | CallKind::Prompt).then(|| {
        div()
            .id("open-call")
            .text_size(px(12.))
            .text_color(t.muted)
            .hover(|s| s.text_color(t.fg))
            .cursor_pointer()
            .on_click(cx.listener(|this, _, window, cx| this.open_call_in_form(window, cx)))
            .child("Open in form")
            .test_support()
    });
    let toolbar = h_flex()
        .h(px(36.))
        .flex_none()
        .px(px(24.))
        .justify_between()
        .border_t_1()
        .border_b_1()
        .border_color(t.hair)
        .child(
            h_flex().gap(px(16.)).children(open_in_form).child(
                div()
                    .id("delete-call")
                    .p(px(2.))
                    .text_color(t.muted)
                    .hover(|s| s.text_color(t.err))
                    .cursor_pointer()
                    .tooltip(|window, cx| Tooltip::new("Delete this call").build(window, cx))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.state.update(cx, |s, cx| s.delete_selected_call(cx));
                    }))
                    .child(Icon::new(IconName::Trash).with_size(px(12.)))
                    .test_support(),
            ),
        )
        .child(
            h_flex()
                .gap(px(10.))
                // Replay needs a session: say so rather than do nothing.
                .when(!connected, |el| {
                    el.child(
                        muted(cx, 11., "Connect to replay")
                            .id("replay-why")
                            .test_support(),
                    )
                })
                .child(kbd(cx, "⌘⏎"))
                .child(
                    accent_button(cx, "Replay", 24.)
                        .id("replay")
                        .when(!connected || pending, |el| el.opacity(0.5).cursor_default())
                        .when(connected && !pending, |el| {
                            el.on_click(
                                cx.listener(|this, _, window, cx| this.perform_call(window, cx)),
                            )
                        })
                        .test_support(),
                ),
        );

    // A replay shown under this row wins over the stored result.
    let shown = match state.response() {
        Some(live) => Shown::live(live).waiting(state.response_waiting()),
        None => {
            let measured = state.server().and_then(|s| s.result_size(&record.id));
            Shown::stored(record, measured)
        }
    };
    let prefix = format!("hist:{}", record.id);
    let revealed = ws.revealed.contains(&prefix);
    // Field by field, so the blob cache can be borrowed mutably beside them.
    let folds = Folds {
        collapsed: &ws.collapsed,
        unfolded: &ws.unfolded,
        toggle: &toggle,
    };
    let sent = Rc::new(record.args.clone());
    let args_tree = json_tree_rc(sent.clone(), &folds, &format!("args:{}", record.id), cx);
    let args = v_flex().px(px(24.)).py(px(16.)).child(tree_section(
        cx,
        "ARGUMENTS",
        SharedString::from(format!("args-copy:{}", record.id)),
        sent,
        args_tree,
    ));
    let mut draw = Draw {
        folds: &folds,
        decoded: &mut ws.decoded,
    };
    Some(
        v_flex()
            .id("detail")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .child(header)
            .child(toolbar)
            .child(args)
            .child(div().flex_1())
            .child(response::render(shown, &prefix, revealed, &mut draw, cx))
            .into_any_element(),
    )
}
