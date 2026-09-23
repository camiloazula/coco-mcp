//! The response section under a detail: header (method · status · elapsed)
//! and the body `content.rs` draws.

use std::borrow::Cow;
use std::time::{Duration, Instant};

use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, TestSupportExt as _, div, px,
};
use mcp_store::{CallRecord, CallStatus};
use serde_json::{Map, Value};

use crate::calls::{Response, ResponseStatus, Waiting};
use crate::clip;
use crate::theme::tokens;
use crate::views::content::{self, Body, Draw};
use crate::views::history::method;
use crate::views::json::{Fit, json_tree};
use crate::views::{Workspace, kbd, mono, muted};

mod labels;

use labels::*;

/// What the response section shows, borrowed from wherever it is kept.
pub struct Shown<'a> {
    method: &'static str,
    raw: Cow<'a, Value>,
    status: ResponseStatus,
    elapsed: Duration,
    /// Where the result breaks the tool's declared output schema.
    issues: Cow<'a, [String]>,
    /// When a request still waiting was sent.
    started: Option<Instant>,
    /// The latest progress it reported, in words.
    progress: Option<String>,
    /// Which answer this is, so text decoded for an earlier one is not reused.
    answer: u64,
}

impl<'a> Shown<'a> {
    /// A response of this session.
    pub fn live(response: &'a Response) -> Self {
        Self {
            method: response.method,
            raw: Cow::Borrowed(&response.raw),
            status: response.status.clone(),
            elapsed: response.elapsed,
            issues: Cow::Borrowed(&response.issues),
            started: None,
            progress: None,
            answer: response.answer,
        }
    }

    /// The running clock and latest progress of `waiting`, the request this
    /// response waits for, if any.
    pub fn waiting(mut self, waiting: Option<&Waiting>) -> Self {
        if let Some(waiting) = waiting {
            self.started = Some(waiting.started);
            self.progress = waiting.progress.as_ref().map(|p| p.label());
        }
        self
    }

    /// A recorded call. A record never changes, and its id is in every key
    /// it is drawn under.
    pub fn stored(record: &'a CallRecord) -> Self {
        let raw = match &record.result {
            Some(result) => Cow::Borrowed(result),
            None => Cow::Owned(Value::Object(Map::new())),
        };
        Self {
            method: method(record.kind),
            raw,
            status: match record.status {
                CallStatus::Ok => ResponseStatus::Ok,
                CallStatus::ToolError => ResponseStatus::ToolError,
                CallStatus::Failed => {
                    ResponseStatus::Failed(record.error.clone().unwrap_or_else(|| "failed".into()))
                }
            },
            elapsed: Duration::from_millis(record.elapsed_ms),
            issues: Cow::Owned(Vec::new()),
            started: None,
            progress: None,
            answer: 0,
        }
    }
}

/// Render the response section.
pub fn render(
    shown: Shown<'_>,
    prefix: &str,
    draw: &mut Draw<'_>,
    open: bool,
    cx: &Context<Workspace>,
) -> AnyElement {
    let t = *tokens(cx);
    draw.decoded.answer(shown.answer);
    let pending = shown.status == ResponseStatus::Pending;
    let elapsed = match (pending, shown.started) {
        (true, Some(started)) => running_label(started.elapsed()),
        (true, None) => "…".to_string(),
        (false, _) => elapsed_label(shown.elapsed),
    };
    let has_body = shown.raw.as_object().is_some_and(|o| !o.is_empty());
    // The header is the toggle, like the log drawer's: a click on it
    // collapses the body to the header or shows it again, and the button at
    // the right does the same for those who look for one.
    let (toggle_icon, toggle_caption) = if open {
        (IconName::PanelBottomClose, "Hide the response")
    } else {
        (IconName::PanelBottomOpen, "Show the response")
    };
    let header = h_flex()
        .id("response-header")
        .h(px(36.))
        .flex_none()
        .px(px(24.))
        .justify_between()
        .cursor_pointer()
        .hover(|s| s.bg(t.hover))
        .on_click(cx.listener(|ws, _, _, cx| {
            ws.state.update(cx, |s, cx| s.toggle_response(cx));
        }))
        .child(
            h_flex()
                .gap(px(12.))
                .items_center()
                .child(
                    div().w(px(10.)).text_color(t.muted).child(
                        Icon::new(if open {
                            gpui_kit::component::IconName::ChevronDown
                        } else {
                            gpui_kit::component::IconName::ChevronRight
                        })
                        .with_size(px(10.))
                        .text_color(t.muted),
                    ),
                )
                .child(div().text_size(px(12.)).child("Response"))
                .child(muted(cx, 11., meta(&shown))),
        )
        .child(
            h_flex()
                .id("response-controls")
                .h_full()
                .gap(px(10.))
                .items_center()
                // A click on a control, or between them, must not reach the
                // header toggle.
                .on_click(|_, _, cx| cx.stop_propagation())
                .children(pending.then(|| {
                    div()
                        .id("cancel-call")
                        .text_size(px(12.))
                        .text_color(t.muted)
                        .hover(|s| s.text_color(t.fg))
                        .cursor_pointer()
                        .child("Cancel")
                        .on_click(cx.listener(|ws, _, _, cx| {
                            ws.state.update(cx, |s, cx| s.cancel_response(cx));
                        }))
                        .test_support()
                }))
                .children(pending.then(|| kbd(cx, "⌘.")))
                .children(has_body.then(|| {
                    clip::icon_button(
                        cx,
                        SharedString::from(format!("{prefix}-copy")),
                        IconName::Copy,
                        "Copy the whole response",
                    )
                    .on_click(cx.listener(|ws, _, _, cx| ws.copy_response(cx)))
                    .test_support()
                }))
                .child(mono(cx, 11., elapsed).text_color(t.muted))
                .child(
                    clip::icon_button(cx, "toggle-response", toggle_icon, toggle_caption)
                        .on_click(cx.listener(|ws, _, _, cx| {
                            ws.state.update(cx, |s, cx| s.toggle_response(cx));
                        }))
                        .test_support(),
                ),
        )
        .test_support();
    if !open {
        // Collapsed: the header alone, under the input, which has the rest.
        return v_flex()
            .id("response")
            .flex_none()
            .border_t_1()
            .border_color(t.hair)
            .child(header)
            .test_support()
            .into_any_element();
    }
    let mut fills = false;
    let body: AnyElement = match &shown.status {
        ResponseStatus::Pending => v_flex()
            .gap(px(4.))
            .child(muted(cx, 12., "Waiting for the server…"))
            .children(shown.progress.clone().map(|progress| {
                muted(cx, 12., format!("Progress {progress}"))
                    .id("call-progress")
                    .test_support()
            }))
            .into_any_element(),
        ResponseStatus::Cancelled => {
            muted(cx, 12., "Cancelled. The server was told to stop.").into_any_element()
        }
        ResponseStatus::Failed(e) => {
            let detail = has_body.then(|| json_tree(&shown.raw, draw.folds, prefix, Fit::Rows, cx));
            v_flex()
                .gap(px(8.))
                .child(
                    div()
                        .id("call-error")
                        .text_size(px(12.))
                        .text_color(t.err)
                        .child(e.clone())
                        .test_support(),
                )
                .children(detail)
                .into_any_element()
        }
        ResponseStatus::Ok | ResponseStatus::ToolError => {
            let Body {
                element,
                fills: full,
            } = content::body(shown.method, &shown.raw, prefix, draw, cx);
            fills = full;
            element
        }
    };
    // Shown above the result, like the form's argument errors: a result the
    // tool's own schema rejects is a finding, not a detail.
    let issues = (!shown.issues.is_empty()).then(|| {
        v_flex()
            .id("output-issues")
            .gap(px(2.))
            .text_size(px(12.))
            .text_color(t.err)
            .child("Checked against the tool's outputSchema:")
            .children(shown.issues.iter().cloned())
            .test_support()
    });
    // The header stays put; the body scrolls under it, apart from the input
    // above the split. A body that fills (one text area) scrolls inside
    // itself instead, and gets the whole height.
    let column = v_flex()
        .px(px(24.))
        .pb(px(20.))
        .gap(px(12.))
        .font_family(cx.theme().mono_font_family.clone())
        .text_size(px(12.))
        .line_height(px(19.2))
        .when(fills, |column| column.size_full().min_h_0())
        .children(issues)
        .child(body);
    v_flex()
        .id("response")
        .size_full()
        .min_h_0()
        .border_t_1()
        .border_color(t.hair)
        .child(header)
        .child(
            div()
                .id("response-body")
                .flex_1()
                .min_h_0()
                .when(!fills, |body| body.overflow_y_scroll())
                .when(fills, |body| body.flex().flex_col())
                .child(column)
                .test_support(),
        )
        .test_support()
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_header_names_the_method_and_the_status() {
        let response = Response {
            method: "tools/call",
            raw: json!({"rows": [1, 2]}),
            status: ResponseStatus::Ok,
            elapsed: Duration::from_millis(4),
            issues: Vec::new(),
            answer: 1,
        };
        assert_eq!(meta(&Shown::live(&response)), "tools/call · OK");
    }

    #[test]
    fn a_waiting_request_runs_a_clock() {
        assert_eq!(running_label(Duration::from_millis(1250)), "1.2 s");
        assert_eq!(running_label(Duration::ZERO), "0.0 s");
        let response = Response {
            method: "tools/call",
            raw: json!({}),
            status: ResponseStatus::Cancelled,
            elapsed: Duration::ZERO,
            issues: Vec::new(),
            answer: 1,
        };
        assert_eq!(meta(&Shown::live(&response)), "tools/call · Cancelled");
        let shown = Shown::live(&response).waiting(None);
        assert!(shown.started.is_none() && shown.progress.is_none());
    }

    fn record(id: &str, result: Option<Value>) -> CallRecord {
        CallRecord {
            id: id.into(),
            server_id: "server".into(),
            kind: mcp_store::CallKind::Resource,
            name: "mock://md/readme".into(),
            args: json!("mock://md/readme"),
            result,
            status: CallStatus::Failed,
            error: None,
            elapsed_ms: 12,
            at: time::OffsetDateTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn a_recorded_call_is_shown_as_it_was_stored() {
        let empty = record("call-1", None);
        let shown = Shown::stored(&empty);
        assert_eq!(shown.method, "resources/read");
        assert_eq!(*shown.raw, json!({}));
        assert_eq!(shown.status, ResponseStatus::Failed("failed".into()));
        assert_eq!(shown.elapsed, Duration::from_millis(12));
        let full = record("call-2", Some(json!({"contents": []})));
        assert_eq!(*Shown::stored(&full).raw, json!({"contents": []}));
    }
}
