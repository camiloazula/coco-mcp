//! Tool calls, resource reads and prompt gets: responses are kept per
//! selection so switching items and back shows the last result.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui_kit::Context;
use mcp_core::ServerSpec;
use mcp_core::{RequestControl, Session};
use mcp_store::{CallKind, CallRecord, CallStatus, NewCall};
use serde_json::{Map, Value};

use crate::state::{AppState, Mode};

/// Outcome of one request.
#[derive(Debug, Clone, PartialEq)]
pub enum ResponseStatus {
    /// In flight.
    Pending,
    /// Answered.
    Ok,
    /// Answered with `isError: true`.
    ToolError,
    /// Stopped by the user; the server was told.
    Cancelled,
    /// Transport or JSON-RPC failure.
    Failed(String),
}

/// The latest `notifications/progress` of a request.
#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    /// The token the request was sent with.
    pub token: Value,
    /// Progress so far.
    pub progress: f64,
    /// Total, if the server knows it.
    pub total: Option<f64>,
    /// What the server says it is doing.
    pub message: Option<String>,
}

impl Progress {
    /// The fields in a few words, as the log drawer words them.
    pub fn label(&self) -> String {
        mcp_core::event::progress_label(self.progress, self.total, self.message.as_deref())
    }
}

/// A request still waiting for its answer.
#[derive(Debug, Clone)]
pub struct Waiting {
    /// The stamp its answer must carry.
    stamp: u64,
    /// Stops it, and names the progress notifications that belong to it.
    control: RequestControl,
    /// When it was sent, for the running clock.
    pub started: Instant,
    /// The latest progress the server reported for it.
    pub progress: Option<Progress>,
}

/// How often the clock of a pending request is redrawn.
const CLOCK_TICK: Duration = Duration::from_millis(100);

/// A response shown under the detail pane.
#[derive(Debug, Clone)]
pub struct Response {
    /// `tools/call`, `resources/read` or `prompts/get`.
    pub method: &'static str,
    /// Raw result JSON (empty object while pending or failed).
    pub raw: Value,
    /// Outcome.
    pub status: ResponseStatus,
    /// Round-trip time.
    pub elapsed: Duration,
    /// Where `structuredContent` breaks the output schema the tool declared,
    /// checked on the runtime thread when the answer arrived.
    pub issues: Vec<String>,
    /// The stamp of the request this answers: a response run again is another
    /// answer, even when its text is the same length at the same address.
    pub answer: u64,
}

/// Key of a response: server id, list mode, item name.
pub type ResponseKey = (String, Mode, String);

/// Per-selection responses.
///
/// One request per key at a time: a second press while the first is in
/// flight is refused. Each request also carries a stamp, and only the answer
/// to the stamp a key is waiting for is written, so an answer that arrives
/// late can never overwrite a newer one.
#[derive(Debug, Default)]
pub struct Responses {
    map: HashMap<ResponseKey, Response>,
    /// The request each pending key is waiting for.
    waiting: HashMap<ResponseKey, Waiting>,
    /// Last stamp handed out.
    stamp: u64,
}

impl Responses {
    /// Look up the response for a key.
    pub fn get(&self, key: &ResponseKey) -> Option<&Response> {
        self.map.get(key)
    }

    /// Whether a request under `key` is still in flight.
    pub fn is_pending(&self, key: &ResponseKey) -> bool {
        self.waiting.contains_key(key)
    }

    /// The request under `key` that is still in flight.
    pub fn waiting(&self, key: &ResponseKey) -> Option<&Waiting> {
        self.waiting.get(key)
    }

    /// Show `key` as pending and hand out the stamp its answer must carry,
    /// with the control that stops it. `None` while an earlier request under
    /// `key` has not been answered.
    pub(crate) fn begin(
        &mut self,
        key: ResponseKey,
        method: &'static str,
    ) -> Option<(u64, RequestControl)> {
        if self.is_pending(&key) {
            return None;
        }
        self.stamp += 1;
        let control = RequestControl::new();
        self.waiting.insert(
            key.clone(),
            Waiting {
                stamp: self.stamp,
                control: control.clone(),
                started: Instant::now(),
                progress: None,
            },
        );
        let raw = Value::Object(Map::new());
        self.map.insert(
            key,
            Response {
                method,
                raw,
                status: ResponseStatus::Pending,
                elapsed: Duration::ZERO,
                issues: Vec::new(),
                answer: self.stamp,
            },
        );
        Some((self.stamp, control))
    }

    /// Whether `stamp` is the request `key` is waiting for.
    pub(crate) fn is_current(&self, key: &ResponseKey, stamp: u64) -> bool {
        self.waiting.get(key).is_some_and(|w| w.stamp == stamp)
    }

    /// Stop the request under `key`. Its answer arrives as
    /// [`ResponseStatus::Cancelled`]. Returns whether one was in flight.
    pub fn cancel(&mut self, key: &ResponseKey) -> bool {
        match self.waiting.get(key) {
            Some(waiting) => {
                waiting.control.cancel();
                true
            }
            None => false,
        }
    }

    /// File `update` under the request sent with its token. Returns whether
    /// one was waiting for it.
    pub fn progress(&mut self, update: Progress) -> bool {
        let found = self
            .waiting
            .values_mut()
            .find(|w| w.control.progress_token().as_ref() == Some(&update.token));
        match found {
            Some(waiting) => {
                waiting.progress = Some(update);
                true
            }
            None => false,
        }
    }

    /// Write the answer to `stamp`. Returns false, and drops the answer, when
    /// `key` is not waiting for that request.
    fn finish(&mut self, key: &ResponseKey, stamp: u64, mut response: Response) -> bool {
        if !self.is_current(key, stamp) {
            return false;
        }
        self.waiting.remove(key);
        response.answer = stamp;
        self.map.insert(key.clone(), response);
        true
    }

    /// Drop server `id`'s responses in `mode`, whose items are gone (History,
    /// once cleared), and stop the requests they wait for.
    pub(crate) fn forget_mode(&mut self, id: &str, mode: Mode) {
        self.map
            .retain(|(server, m, _), _| !(server == id && *m == mode));
        self.waiting.retain(|(server, m, _), waiting| {
            let kept = !(server == id && *m == mode);
            if !kept {
                waiting.control.cancel();
            }
            kept
        });
    }

    /// Drop every response of server `id`, which was deleted, and every
    /// request it was waiting for, so an answer still on its way is refused.
    /// Drop what is kept for one recorded call of server `id`.
    pub(crate) fn forget_call(&mut self, id: &str, call: &str) {
        let of_call = |(server, mode, name): &ResponseKey| {
            server == id && *mode == Mode::History && name == call
        };
        self.map.retain(|key, _| !of_call(key));
        self.waiting.retain(|key, waiting| {
            let kept = !of_call(key);
            if !kept {
                waiting.control.cancel();
            }
            kept
        });
    }

    pub(crate) fn forget_server(&mut self, id: &str) {
        self.map.retain(|(server, _, _), _| server != id);
        self.waiting.retain(|(server, _, _), waiting| {
            let kept = server != id;
            if !kept {
                waiting.control.cancel();
            }
            kept
        });
    }
}

/// What a request yields: the raw result, the round trip and `isError`.
type Answer = Result<(Value, Duration, bool), mcp_core::Error>;

/// Result of a bridge-executed request.
type CallResult = Option<Result<(Value, Duration, bool), mcp_core::Error>>;

/// `fut`, a `method` request, settled where it runs, on the runtime: the
/// response it shows, checked against `output_schema` when the tool declared
/// one, and, when `record` and there is anything to record, the copy of its
/// result the history row keeps. A large result is never cloned or validated
/// on the UI thread.
async fn settled(
    method: &'static str,
    record: bool,
    output_schema: Option<Value>,
    spec: Option<ServerSpec>,
    fut: impl Future<Output = Answer>,
) -> (Response, Option<Value>) {
    let mut response = outcome(method, Some(fut.await), spec.as_ref());
    if let Some(schema) = &output_schema
        && response.status == ResponseStatus::Ok
    {
        response.issues = output_issues(schema, &response.raw);
    }
    // Recorded whenever there is anything to record, so a failure's error
    // data survives into History too.
    let result = (record && !matches!(&response.raw, Value::Object(o) if o.is_empty()))
        .then(|| response.raw.clone());
    (response, result)
}

/// Where a tool result breaks the output schema the tool declared: no
/// `structuredContent` at all, or content the schema rejects.
fn output_issues(schema: &Value, raw: &Value) -> Vec<String> {
    match raw.get("structuredContent") {
        None => vec!["no structuredContent, though the tool declares an outputSchema".to_owned()],
        Some(content) => mcp_schema_form::validate(schema, content)
            .iter()
            .map(ToString::to_string)
            .collect(),
    }
}

/// The response a finished request shows; `spec` is the server asked, for
/// the wording of a failure.
fn outcome(method: &'static str, result: CallResult, spec: Option<&ServerSpec>) -> Response {
    let (raw, status, elapsed) = match result {
        Some(Ok((raw, elapsed, is_error))) => (
            raw,
            if is_error {
                ResponseStatus::ToolError
            } else {
                ResponseStatus::Ok
            },
            elapsed,
        ),
        Some(Err(mcp_core::Error::Cancelled)) => (
            Value::Object(Map::new()),
            ResponseStatus::Cancelled,
            Duration::ZERO,
        ),
        // Keep a server error's structured `data`: the response view shows
        // it as a tree under the message.
        Some(Err(e)) => {
            // Only when the server sent structured `data`: the code and
            // message are already on the red line above.
            let raw = match &e {
                mcp_core::Error::Server {
                    code,
                    message,
                    data: Some(data),
                } => serde_json::json!({
                    "code": code,
                    "message": message,
                    "data": data,
                }),
                _ => Value::Object(Map::new()),
            };
            (
                raw,
                ResponseStatus::Failed(crate::explain::explain(&e, spec)),
                Duration::ZERO,
            )
        }
        None => (
            Value::Object(Map::new()),
            ResponseStatus::Failed("call task failed".into()),
            Duration::ZERO,
        ),
    };
    Response {
        method,
        raw,
        status,
        elapsed,
        issues: Vec::new(),
        answer: 0,
    }
}

/// What is being sent, for the response header and the history row.
struct Request {
    method: &'static str,
    kind: CallKind,
    name: String,
    args: Value,
    /// Whether the call is written to history: not for a read the app made
    /// by itself, such as refreshing a subscribed resource.
    record: bool,
    /// The tool's declared output schema, which its result is checked against.
    output_schema: Option<Value>,
}

impl AppState {
    /// Key of the current selection.
    pub fn response_key(&self) -> Option<ResponseKey> {
        let server = self.server()?;
        Some((server.record.id.clone(), self.mode, self.selected_name()?))
    }

    /// Response for the current selection.
    pub fn response(&self) -> Option<&Response> {
        let key = self.response_key()?;
        self.responses.get(&key)
    }

    /// Whether the current selection's request is still in flight; the call
    /// controls are disabled until it is answered.
    pub fn response_pending(&self) -> bool {
        self.response_key()
            .is_some_and(|key| self.responses.is_pending(&key))
    }

    /// The current selection's request while it is in flight.
    pub fn response_waiting(&self) -> Option<&Waiting> {
        self.responses.waiting(&self.response_key()?)
    }

    /// Stop the current selection's request; the server is told, and the
    /// response reads as cancelled once the session gives the call up.
    pub fn cancel_response(&mut self, cx: &mut Context<Self>) {
        if let Some(key) = self.response_key()
            && self.responses.cancel(&key)
        {
            self.changed(cx);
        }
    }

    fn session(&self) -> Option<Session> {
        self.server().and_then(|s| s.session.clone())
    }

    /// Key under which a request for `name` in the current mode is shown.
    fn key_for(&self, name: &str) -> Option<ResponseKey> {
        let server = self.server()?;
        Some((server.record.id.clone(), self.mode, name.to_owned()))
    }

    /// `tools/call`.
    pub fn call_tool(&mut self, name: String, args: Value, cx: &mut Context<Self>) {
        if let Some(key) = self.key_for(&name) {
            self.call_tool_as(key, name, args, cx);
        }
    }

    fn call_tool_as(
        &mut self,
        key: ResponseKey,
        name: String,
        args: Value,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.session() else {
            return;
        };
        let tool = name.clone();
        let sent = args.clone();
        let output_schema = self
            .snapshot()
            .and_then(|s| s.tool(&name))
            .and_then(|t| t.output_schema.clone());
        self.start(
            Request {
                method: "tools/call",
                kind: CallKind::Tool,
                name,
                args: sent,
                record: true,
                output_schema,
            },
            key,
            move |control| async move {
                session
                    .call_tool_with(&tool, args, &control)
                    .await
                    .map(|o| (o.raw, o.elapsed, o.is_error))
            },
            cx,
        );
    }

    /// `resources/read`. `name` is the list entry (URI or template); `uri` is what is read.
    pub fn read_resource(&mut self, name: String, uri: String, cx: &mut Context<Self>) {
        if let Some(key) = self.key_for(&name) {
            self.read_resource_as(key, name, uri, cx);
        }
    }

    fn read_resource_as(
        &mut self,
        key: ResponseKey,
        name: String,
        uri: String,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.session() else {
            return;
        };
        self.start_read(session, key, name, uri, true, cx);
    }

    /// Read server `server_id`'s resource `uri` again, when a response for it
    /// is on record and not waiting: the server said the subscribed resource
    /// changed. Nobody asked for the read, so it is not recorded in history.
    pub(crate) fn reread_resource(&mut self, server_id: &str, uri: &str, cx: &mut Context<Self>) {
        let key: ResponseKey = (server_id.to_owned(), Mode::Resources, uri.to_owned());
        if self.responses.get(&key).is_none() || self.responses.is_pending(&key) {
            return;
        }
        let Some(session) = self
            .servers
            .iter()
            .find(|s| s.record.id == server_id)
            .and_then(|s| s.session.clone())
        else {
            return;
        };
        self.start_read(session, key, uri.to_owned(), uri.to_owned(), false, cx);
    }

    fn start_read(
        &mut self,
        session: Session,
        key: ResponseKey,
        name: String,
        uri: String,
        record: bool,
        cx: &mut Context<Self>,
    ) {
        let target = uri.clone();
        self.start(
            Request {
                method: "resources/read",
                kind: CallKind::Resource,
                name,
                args: Value::String(uri),
                record,
                output_schema: None,
            },
            key,
            move |control| async move {
                session
                    .read_resource_with(&target, &control)
                    .await
                    .map(|o| (o.raw, o.elapsed, false))
            },
            cx,
        );
    }

    /// `prompts/get`.
    pub fn get_prompt(&mut self, name: String, args: Map<String, Value>, cx: &mut Context<Self>) {
        if let Some(key) = self.key_for(&name) {
            self.get_prompt_as(key, name, args, cx);
        }
    }

    fn get_prompt_as(
        &mut self,
        key: ResponseKey,
        name: String,
        args: Map<String, Value>,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.session() else {
            return;
        };
        let prompt = name.clone();
        let sent = Value::Object(args.clone());
        self.start(
            Request {
                method: "prompts/get",
                kind: CallKind::Prompt,
                name,
                args: sent,
                record: true,
                output_schema: None,
            },
            key,
            move |control| async move {
                session
                    .get_prompt_with(&prompt, args, &control)
                    .await
                    .map(|o| (o.raw, o.elapsed, false))
            },
            cx,
        );
    }

    /// Send a recorded call again. The new response is shown under the
    /// history row and the call is recorded like any other.
    pub fn replay(&mut self, record: &CallRecord, cx: &mut Context<Self>) {
        let Some(server) = self.server() else {
            return;
        };
        if server.record.id != record.server_id {
            return;
        }
        let key: ResponseKey = (record.server_id.clone(), Mode::History, record.id.clone());
        match record.kind {
            CallKind::Tool => self.call_tool_as(key, record.name.clone(), record.args.clone(), cx),
            CallKind::Resource => {
                let uri = record
                    .args
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| record.name.clone());
                self.read_resource_as(key, record.name.clone(), uri, cx);
            }
            CallKind::Prompt => {
                let args = record.args.as_object().cloned().unwrap_or_default();
                self.get_prompt_as(key, record.name.clone(), args, cx);
            }
        }
    }

    /// Send the request `send` builds with the control that stops it, show it
    /// as pending under `key` with a running clock, and file its answer.
    fn start<F>(
        &mut self,
        request: Request,
        key: ResponseKey,
        send: impl FnOnce(RequestControl) -> F,
        cx: &mut Context<Self>,
    ) where
        F: Future<Output = Answer> + Send + 'static,
    {
        let Some(bridge) = self.bridge.clone() else {
            return;
        };
        let Request {
            method,
            kind,
            name,
            args,
            record,
            output_schema,
        } = request;
        let Some((stamp, control)) = self.responses.begin(key.clone(), method) else {
            return;
        };
        // What was asked for is about to arrive: a collapsed panel opens.
        self.response_open = true;
        let server_id = key.0.clone();
        let not_recorded = format!("call to `{name}` was not recorded");
        let record = record && self.store.is_some();
        self.changed(cx);

        // The clock of a pending request moves, so it is redrawn while the
        // request waits, and not once it is answered.
        let ticking = key.clone();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(CLOCK_TICK).await;
                let pending = this.update(cx, |state, cx| {
                    let pending = state.responses.is_current(&ticking, stamp);
                    if pending {
                        cx.notify();
                    }
                    pending
                });
                if !matches!(pending, Ok(true)) {
                    break;
                }
            }
        })
        .detach();

        let spec = self
            .servers
            .iter()
            .find(|s| s.record.id == server_id)
            .map(|s| s.record.spec.clone());
        let unanswered = spec.clone();
        let run = bridge.run(settled(method, record, output_schema, spec, send(control)));
        cx.spawn(async move |this, cx| {
            let (response, result) = run
                .await
                .unwrap_or_else(|| (outcome(method, None, unanswered.as_ref()), None));
            let answered = this.update(cx, |state, _| {
                if !state.responses.is_current(&key, stamp) {
                    return None;
                }
                let elapsed_ms = u64::try_from(response.elapsed.as_millis()).unwrap_or(u64::MAX);
                // A server deleted while the request was out has no row to
                // record against.
                let call = state
                    .store
                    .clone()
                    .filter(|_| record)
                    .filter(|_| state.servers.iter().any(|s| s.record.id == server_id))
                    .map(|store| {
                        let call = NewCall {
                            server_id: server_id.clone(),
                            kind,
                            name,
                            args,
                            result,
                            status: match &response.status {
                                ResponseStatus::Ok => CallStatus::Ok,
                                ResponseStatus::ToolError => CallStatus::ToolError,
                                _ => CallStatus::Failed,
                            },
                            error: match &response.status {
                                ResponseStatus::Failed(e) => Some(e.clone()),
                                _ => None,
                            },
                            elapsed_ms,
                        };
                        (store, call)
                    });
                Some((response, elapsed_ms, call))
            });
            let Ok(Some((response, elapsed_ms, call))) = answered else {
                return;
            };
            // The database write runs on the blocking pool; the response and
            // its history row still appear together, in the update below.
            let recorded = match call {
                Some((store, call)) => Some(
                    bridge
                        .run_blocking(move || store.record_call(call).map_err(|e| e.to_string()))
                        .await
                        .unwrap_or_else(|| Err("call recording task failed".into())),
                ),
                None => None,
            };
            let _ = this.update(cx, |state, cx| {
                let answered = matches!(
                    response.status,
                    ResponseStatus::Ok | ResponseStatus::ToolError
                );
                if !state.responses.finish(&key, stamp, response) {
                    return;
                }
                let known = state.servers.iter().any(|s| s.record.id == server_id);
                match recorded {
                    Some(Ok(record)) => state.push_history(record),
                    Some(Err(e)) if known => state.note_failure(&not_recorded, e),
                    _ => {}
                }
                if let Some(entry) = state.servers.iter_mut().find(|s| s.record.id == server_id)
                    && answered
                {
                    entry.last_call_ms = Some(elapsed_ms);
                }
                state.changed(cx);
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> ResponseKey {
        ("server".into(), Mode::Tools, "sleep".into())
    }

    fn answer(text: &str) -> Response {
        let raw = serde_json::json!({ "content": [{ "type": "text", "text": text }] });
        outcome(
            "tools/call",
            Some(Ok((raw.clone(), Duration::from_millis(1), false))),
            None,
        )
    }

    fn text(responses: &Responses) -> Option<&str> {
        responses.get(&key())?.raw["content"][0]["text"].as_str()
    }

    #[test]
    fn a_second_request_waits_for_the_first() {
        let mut responses = Responses::default();
        let (first, _) = responses.begin(key(), "tools/call").unwrap();
        assert!(responses.is_pending(&key()));
        assert!(responses.begin(key(), "tools/call").is_none());
        assert!(responses.is_pending(&key()), "still waiting for the first");
        assert_eq!(
            responses.get(&key()).map(|r| r.status.clone()),
            Some(ResponseStatus::Pending)
        );

        // Another selection is a different request and may run alongside.
        let other: ResponseKey = ("server".into(), Mode::History, "call-1".into());
        assert!(responses.begin(other.clone(), "tools/call").is_some());

        assert!(responses.finish(&key(), first, answer("first")));
        assert!(!responses.is_pending(&key()));
        assert!(responses.is_pending(&other));
        let (second, _) = responses.begin(key(), "tools/call").unwrap();
        assert_ne!(first, second);
    }

    #[test]
    fn a_late_answer_does_not_overwrite_a_newer_one() {
        let mut responses = Responses::default();
        let (first, _) = responses.begin(key(), "tools/call").unwrap();
        assert!(responses.finish(&key(), first, answer("first")));
        assert_eq!(text(&responses), Some("first"));

        let (second, _) = responses.begin(key(), "tools/call").unwrap();
        assert!(!responses.is_current(&key(), first));
        assert!(
            !responses.finish(&key(), first, answer("stale")),
            "an answer to an older request is dropped"
        );
        assert!(responses.is_pending(&key()));
        assert_eq!(
            responses.get(&key()).map(|r| r.status.clone()),
            Some(ResponseStatus::Pending)
        );

        assert!(responses.finish(&key(), second, answer("second")));
        assert_eq!(text(&responses), Some("second"));
        assert!(
            !responses.finish(&key(), second, answer("again")),
            "a request is answered once"
        );
        assert_eq!(text(&responses), Some("second"));
    }

    #[test]
    fn a_deleted_server_takes_only_its_own_responses() {
        let mut responses = Responses::default();
        let other: ResponseKey = ("kept".into(), Mode::Tools, "sleep".into());
        let (answered, _) = responses.begin(key(), "tools/call").unwrap();
        assert!(responses.finish(&key(), answered, answer("first")));
        let (waiting, stopped) = responses.begin(key(), "tools/call").unwrap();
        let history: ResponseKey = ("server".into(), Mode::History, "call-1".into());
        responses.begin(history.clone(), "tools/call").unwrap();
        let (kept, running) = responses.begin(other.clone(), "tools/call").unwrap();

        responses.forget_server("server");
        assert!(stopped.is_cancelled(), "a forgotten request is stopped");
        assert!(!running.is_cancelled());
        assert!(responses.get(&key()).is_none());
        assert!(!responses.is_pending(&key()) && !responses.is_pending(&history));
        assert!(
            !responses.finish(&key(), waiting, answer("late")),
            "an answer to a forgotten request is refused"
        );
        assert!(responses.get(&key()).is_none(), "and not written");
        assert!(responses.is_pending(&other), "another server keeps its own");
        assert!(responses.finish(&other, kept, answer("kept")));
        assert!(responses.get(&other).is_some());
    }

    #[test]
    fn a_cancelled_request_is_stopped_through_its_control() {
        let mut responses = Responses::default();
        assert!(!responses.cancel(&key()), "nothing to stop");
        let (stamp, control) = responses.begin(key(), "tools/call").unwrap();
        assert!(responses.cancel(&key()));
        assert!(control.is_cancelled());
        let stopped = outcome("tools/call", Some(Err(mcp_core::Error::Cancelled)), None);
        assert_eq!(stopped.status, ResponseStatus::Cancelled);
        assert!(responses.finish(&key(), stamp, stopped));
        assert!(!responses.is_pending(&key()));
        assert!(
            !responses.cancel(&key()),
            "an answered request is not stopped again"
        );
    }

    #[test]
    fn progress_is_filed_under_the_request_it_names() {
        let mut responses = Responses::default();
        let (_, control) = responses.begin(key(), "tools/call").unwrap();
        let update = |token: Value| Progress {
            token,
            progress: 2.0,
            total: Some(5.0),
            message: Some("copying".into()),
        };
        // The token is known once the session sent the request.
        assert!(!responses.progress(update(serde_json::json!(7))));
        assert!(control.progress_token().is_none());
        assert!(responses.waiting(&key()).unwrap().progress.is_none());
        let label = update(serde_json::json!(7)).label();
        assert_eq!(label, "2 (40%): copying");
    }

    #[test]
    fn a_result_is_checked_against_the_declared_output_schema() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {"value": {"type": "number"}},
            "required": ["value"],
        });
        let run = |raw: Value| {
            futures::executor::block_on(settled(
                "tools/call",
                false,
                Some(schema.clone()),
                None,
                async move { Ok((raw, Duration::ZERO, false)) },
            ))
            .0
        };
        assert!(
            run(serde_json::json!({"content": [], "structuredContent": {"value": 3}}))
                .issues
                .is_empty()
        );
        let wrong = run(serde_json::json!({"content": [], "structuredContent": {"value": "3"}}));
        assert_eq!(wrong.issues.len(), 1, "{:?}", wrong.issues);
        assert!(wrong.issues[0].starts_with("$.value"), "{:?}", wrong.issues);
        let missing = run(serde_json::json!({"content": [{"type": "text", "text": "3"}]}));
        assert_eq!(
            missing.issues,
            ["no structuredContent, though the tool declares an outputSchema"]
        );
        // A tool error is not held to the schema.
        let error = futures::executor::block_on(settled(
            "tools/call",
            false,
            Some(schema.clone()),
            None,
            async move { Ok((serde_json::json!({"content": []}), Duration::ZERO, true)) },
        ))
        .0;
        assert!(error.issues.is_empty());
    }

    #[test]
    fn a_recorded_result_is_copied_where_it_is_answered() {
        let raw = serde_json::json!({"content": [{"type": "text", "text": "clear"}]});
        let run =
            |record: bool, answer: Answer| {
                futures::executor::block_on(settled("tools/call", record, None, None, async move {
                    answer
                }))
            };
        let (response, result) = run(true, Ok((raw.clone(), Duration::from_millis(2), false)));
        assert_eq!(response.raw, raw);
        assert_eq!(result, Some(raw.clone()), "the history row's own copy");
        let (_, unrecorded) = run(false, Ok((raw.clone(), Duration::ZERO, false)));
        assert_eq!(unrecorded, None, "nothing is copied without a database");
        let (_, empty) = run(true, Ok((serde_json::json!({}), Duration::ZERO, false)));
        assert_eq!(empty, None, "nothing to record");
        let (failed, data) = run(
            true,
            Err(mcp_core::Error::Server {
                code: -32602,
                message: "bad arguments".into(),
                data: Some(serde_json::json!({"field": "city"})),
            }),
        );
        assert!(matches!(failed.status, ResponseStatus::Failed(_)));
        assert_eq!(data, Some(failed.raw), "a failure's error data is recorded");
    }
}
