//! The `rmcp` client handler: answers server-initiated requests according to
//! a per-server [`ServerRequestPolicy`] and turns server notifications into
//! semantic events.
//!
//! A legacy server sends sampling, elicitation and roots requests of its own;
//! a 2026-07-28 server asks for them inside a round trip instead. Both are
//! answered by `CocoClient::answer`, so the policy and the dialog are
//! the same in either era.

// Sampling and roots are deprecated by SEP-2577 but still widely used by
// servers; a debugger has to support them.
#![allow(deprecated)]

use std::time::Duration;

use rmcp::model::{
    CancelledNotificationParam, ClientCapabilities, ClientInfo, CreateMessageRequestParams,
    CreateMessageResult, ElicitRequestParams, ElicitResult,
    ElicitationAction as RmcpElicitationAction, ErrorData, Implementation, ListRootsResult,
    LoggingMessageNotificationParam, ProgressNotificationParam, ProtocolVersion,
    ResourceUpdatedNotificationParam,
};
use rmcp::service::{NotificationContext, RequestContext};
use rmcp::{ClientHandler, RoleClient};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::event::{
    ElicitationMode, EventKind, EventSink, ListKind, Root, ServerRequest, ServerRequestKind,
    ServerResponse,
};

/// What to do when the server sends `sampling/createMessage`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum SamplingPolicy {
    /// Return a JSON-RPC error.
    Reject,
    /// Emit a [`ServerRequest`] event and wait for the UI to answer.
    Prompt,
    /// Answer immediately with a fixed assistant message.
    Auto {
        /// Model name to report.
        model: String,
        /// Text of the assistant reply.
        text: String,
    },
}

/// What to do when the server sends `elicitation/create`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ElicitationPolicy {
    /// Decline without asking.
    Decline,
    /// Emit a [`ServerRequest`] event and wait for the UI to answer.
    Prompt,
}

/// What to do when the server sends `roots/list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum RootsPolicy {
    /// Always return this list (possibly empty).
    Fixed {
        /// Roots to advertise.
        roots: Vec<Root>,
    },
    /// Emit a [`ServerRequest`] event and wait for the UI to answer.
    Prompt,
}

/// Per-server configuration for server-initiated requests.
///
/// The default puts a person in the loop: sampling, elicitation and roots
/// are all `Prompt`, so every server-initiated request is shown to whoever
/// listens to the session's events instead of being auto-answered. A
/// session nobody listens to falls back at once (sampling is refused,
/// elicitation declined, no roots), so the default never stalls a caller
/// without a listener. A caller that listens but has nobody to ask, such as
/// a terminal printing the event log, sets `Reject`, `Decline` and `Fixed`
/// itself rather than waiting out `prompt_timeout`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerRequestPolicy {
    /// Sampling behaviour.
    pub sampling: SamplingPolicy,
    /// Elicitation behaviour.
    pub elicitation: ElicitationPolicy,
    /// Roots behaviour.
    pub roots: RootsPolicy,
    /// How long a `Prompt` waits for the UI before falling back.
    #[serde(with = "secs")]
    pub prompt_timeout: Duration,
}

mod secs {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        d.as_secs().serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        u64::deserialize(d).map(Duration::from_secs)
    }
}

impl Default for ServerRequestPolicy {
    fn default() -> Self {
        Self {
            sampling: SamplingPolicy::Prompt,
            elicitation: ElicitationPolicy::Prompt,
            roots: RootsPolicy::Prompt,
            prompt_timeout: Duration::from_secs(120),
        }
    }
}

/// Client-side handler installed in every session.
#[derive(Debug, Clone)]
pub struct CocoClient {
    sink: EventSink,
    policy: ServerRequestPolicy,
    info: ClientInfo,
}

impl CocoClient {
    /// Create a handler reporting to `sink`.
    pub fn new(sink: EventSink, policy: ServerRequestPolicy, client: Implementation) -> Self {
        // The typed builder is only compiled with rmcp's `server`/`macros`
        // features; the JSON form is the wire format and needs neither.
        let capabilities: ClientCapabilities = serde_json::from_value(json!({
            "sampling": {},
            // Both modes: a server may only send a URL elicitation to a
            // client that declared it.
            "elicitation": {"form": {}, "url": {}},
            "roots": {"listChanged": true},
        }))
        .unwrap_or_default();
        Self {
            sink,
            policy,
            // What the handshake offers; `server/discover` asks for the
            // versions its lifecycle names instead.
            info: ClientInfo::new(capabilities, client)
                .with_protocol_version(ProtocolVersion::V_2025_11_25),
        }
    }

    fn policy(&self) -> ServerRequestPolicy {
        self.policy.clone()
    }

    /// Emit a `ServerRequest` event and wait for an answer. Returns `None`
    /// when nobody is listening, the wait timed out, or the request was
    /// withdrawn (`cancelled`), so the caller applies its fallback.
    async fn ask(
        &self,
        id: Value,
        kind: ServerRequestKind,
        timeout: Duration,
        cancelled: &CancellationToken,
    ) -> Option<ServerResponse> {
        if self.sink.receiver_count() == 0 {
            return None;
        }
        let (request, rx) = ServerRequest::new(id, kind);
        self.sink.emit(EventKind::ServerRequest(request));
        tokio::select! {
            answer = tokio::time::timeout(timeout, rx) => match answer {
                Ok(Ok(response)) => Some(response),
                _ => None,
            },
            () = cancelled.cancelled() => None,
        }
    }

    /// Answer a request for `kind` the way the policy says, asking a person
    /// where it says to: the JSON result the server is sent, or the error it
    /// is refused with. `id` names the request to whoever answers it;
    /// `cancelled` withdraws it.
    pub(crate) async fn answer(
        &self,
        id: Value,
        kind: ServerRequestKind,
        cancelled: &CancellationToken,
    ) -> Result<Value, ErrorData> {
        let policy = self.policy();
        match kind {
            ServerRequestKind::Sampling(params) => match policy.sampling {
                SamplingPolicy::Reject => Err(rejected("sampling rejected by client policy")),
                SamplingPolicy::Auto { model, text } => Ok(json!({
                    "model": model,
                    "role": "assistant",
                    "content": {"type": "text", "text": text},
                    "stopReason": "endTurn",
                })),
                SamplingPolicy::Prompt => {
                    let kind = ServerRequestKind::Sampling(params);
                    match self.ask(id, kind, policy.prompt_timeout, cancelled).await {
                        Some(ServerResponse::Sampling(value)) => {
                            serde_json::from_value::<CreateMessageResult>(value.clone()).map_err(
                                |e| {
                                    ErrorData::invalid_params(
                                        format!("bad sampling result: {e}"),
                                        None,
                                    )
                                },
                            )?;
                            Ok(value)
                        }
                        Some(ServerResponse::Reject { message }) => Err(rejected(message)),
                        Some(_) => Err(rejected("mismatched answer for sampling request")),
                        None => Err(rejected("sampling request unanswered")),
                    }
                }
            },
            kind @ ServerRequestKind::Elicitation { .. } => {
                let decline = json!({"action": "decline"});
                match policy.elicitation {
                    ElicitationPolicy::Decline => Ok(decline),
                    ElicitationPolicy::Prompt => {
                        match self.ask(id, kind, policy.prompt_timeout, cancelled).await {
                            Some(ServerResponse::Elicitation { action, content }) => {
                                let mut result = json!({ "action": action });
                                if let Some(content) = content {
                                    result["content"] = content;
                                }
                                Ok(result)
                            }
                            Some(ServerResponse::Reject { message }) => Err(rejected(message)),
                            _ => Ok(decline),
                        }
                    }
                }
            }
            ServerRequestKind::ListRoots => match policy.roots {
                RootsPolicy::Fixed { roots } => Ok(json!({ "roots": roots })),
                RootsPolicy::Prompt => {
                    let kind = ServerRequestKind::ListRoots;
                    match self.ask(id, kind, policy.prompt_timeout, cancelled).await {
                        Some(ServerResponse::Roots(roots)) => Ok(json!({ "roots": roots })),
                        Some(ServerResponse::Reject { message }) => Err(rejected(message)),
                        _ => Ok(json!({ "roots": [] })),
                    }
                }
            },
        }
    }
}

/// What an input request of a round trip asks for, read from its JSON rather
/// than from `rmcp`'s types, so a field the 2026-07-28 revision dropped (a URL
/// elicitation's `elicitationId`) is not required.
pub(crate) fn input_request_kind(request: &Value) -> Result<ServerRequestKind, String> {
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let params = request.get("params").cloned().unwrap_or(Value::Null);
    match method {
        "sampling/createMessage" => Ok(ServerRequestKind::Sampling(params)),
        "elicitation/create" => {
            let message = params
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let url = params.get("url").and_then(Value::as_str);
            let mode = match (params.get("mode").and_then(Value::as_str), url) {
                (Some("url"), Some(url)) | (None, Some(url)) => ElicitationMode::Url {
                    url: web_url(url)?,
                    elicitation_id: params
                        .get("elicitationId")
                        .and_then(Value::as_str)
                        .filter(|id| !id.is_empty())
                        .map(str::to_owned),
                },
                (Some("url"), None) => {
                    return Err("the server asked to open a URL without giving one".into());
                }
                _ => ElicitationMode::Form {
                    schema: params
                        .get("requestedSchema")
                        .cloned()
                        .unwrap_or(Value::Null),
                },
            };
            Ok(ServerRequestKind::Elicitation { message, mode })
        }
        "roots/list" => Ok(ServerRequestKind::ListRoots),
        other => Err(format!(
            "the server asked for `{other}`, which a round trip cannot carry"
        )),
    }
}

/// A URL a server asks the client to open, admitted only when a browser is
/// what would open it: `http` or `https`, nothing that launches another
/// program.
fn web_url(url: &str) -> std::result::Result<String, String> {
    match url::Url::parse(url) {
        Ok(parsed) if matches!(parsed.scheme(), "http" | "https") => Ok(url.to_owned()),
        Ok(parsed) => Err(format!(
            "the server asked to open a `{}:` URL; only http and https are opened",
            parsed.scheme()
        )),
        Err(e) => Err(format!(
            "the server asked to open `{url}`, which is not a URL: {e}"
        )),
    }
}

fn request_id(context: &RequestContext<RoleClient>) -> Value {
    serde_json::to_value(&context.id).unwrap_or(Value::Null)
}

fn rejected(message: impl Into<String>) -> ErrorData {
    ErrorData::new(
        rmcp::model::ErrorCode::INVALID_REQUEST,
        message.into(),
        None,
    )
}

/// An answer as the typed result `rmcp` sends back.
fn typed<T: DeserializeOwned>(answer: Value) -> Result<T, ErrorData> {
    serde_json::from_value(answer).map_err(|e| ErrorData::internal_error(e.to_string(), None))
}

impl ClientHandler for CocoClient {
    fn get_info(&self) -> ClientInfo {
        self.info.clone()
    }

    async fn create_message(
        &self,
        params: CreateMessageRequestParams,
        context: RequestContext<RoleClient>,
    ) -> Result<CreateMessageResult, ErrorData> {
        let raw = serde_json::to_value(&params).unwrap_or(Value::Null);
        let kind = ServerRequestKind::Sampling(raw);
        typed(self.answer(request_id(&context), kind, &context.ct).await?)
    }

    async fn create_elicitation(
        &self,
        request: ElicitRequestParams,
        context: RequestContext<RoleClient>,
    ) -> Result<ElicitResult, ErrorData> {
        let (message, mode) = match request {
            ElicitRequestParams::FormElicitationParams {
                message,
                requested_schema,
                ..
            } => (
                message,
                ElicitationMode::Form {
                    schema: serde_json::to_value(requested_schema).unwrap_or(Value::Null),
                },
            ),
            ElicitRequestParams::UrlElicitationParams {
                message,
                url,
                elicitation_id,
                ..
            } => {
                // A URL the client would open must be one for a browser; a
                // `file:` or custom scheme is declined, not opened.
                let Ok(url) = web_url(&url) else {
                    return Ok(ElicitResult::new(RmcpElicitationAction::Decline));
                };
                (
                    message,
                    ElicitationMode::Url {
                        url,
                        elicitation_id: Some(elicitation_id).filter(|id| !id.is_empty()),
                    },
                )
            }
            _ => return Ok(ElicitResult::new(RmcpElicitationAction::Decline)),
        };
        let kind = ServerRequestKind::Elicitation { message, mode };
        typed(self.answer(request_id(&context), kind, &context.ct).await?)
    }

    async fn list_roots(
        &self,
        context: RequestContext<RoleClient>,
    ) -> Result<ListRootsResult, ErrorData> {
        let kind = ServerRequestKind::ListRoots;
        typed(self.answer(request_id(&context), kind, &context.ct).await?)
    }

    async fn on_cancelled(
        &self,
        params: CancelledNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        self.sink.emit(EventKind::RequestCancelled {
            id: params
                .request_id
                .and_then(|id| serde_json::to_value(id).ok())
                .unwrap_or(Value::Null),
            reason: params.reason,
        });
    }

    async fn on_logging_message(
        &self,
        params: LoggingMessageNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        let level = serde_json::to_value(params.level)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_else(|| "info".into());
        self.sink.emit(EventKind::Log {
            level,
            logger: params.logger,
            data: params.data,
        });
    }

    async fn on_progress(
        &self,
        params: ProgressNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        self.sink.emit(EventKind::Progress {
            token: serde_json::to_value(&params.progress_token).unwrap_or(Value::Null),
            progress: params.progress,
            total: params.total,
            message: params.message,
        });
    }

    async fn on_resource_updated(
        &self,
        params: ResourceUpdatedNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        self.sink
            .emit(EventKind::ResourceUpdated { uri: params.uri });
    }

    async fn on_resource_list_changed(&self, _context: NotificationContext<RoleClient>) {
        self.sink.emit(EventKind::ListChanged(ListKind::Resources));
    }

    async fn on_tool_list_changed(&self, _context: NotificationContext<RoleClient>) {
        self.sink.emit(EventKind::ListChanged(ListKind::Tools));
    }

    async fn on_prompt_list_changed(&self, _context: NotificationContext<RoleClient>) {
        self.sink.emit(EventKind::ListChanged(ListKind::Prompts));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::ElicitationAction;

    #[test]
    fn policy_round_trips_through_json() {
        let policy = ServerRequestPolicy {
            sampling: SamplingPolicy::Auto {
                model: "m".into(),
                text: "t".into(),
            },
            elicitation: ElicitationPolicy::Prompt,
            roots: RootsPolicy::Fixed {
                roots: vec![Root {
                    uri: "file:///tmp".into(),
                    name: None,
                }],
            },
            prompt_timeout: Duration::from_secs(5),
        };
        let json = serde_json::to_value(&policy).unwrap();
        assert_eq!(json["sampling"]["mode"], "auto");
        assert_eq!(json["prompt_timeout"], 5);
        let back: ServerRequestPolicy = serde_json::from_value(json).unwrap();
        assert_eq!(back, policy);
    }

    #[test]
    fn the_default_policy_asks_a_person() {
        let policy = ServerRequestPolicy::default();
        assert_eq!(policy.sampling, SamplingPolicy::Prompt);
        assert_eq!(policy.elicitation, ElicitationPolicy::Prompt);
        assert_eq!(policy.roots, RootsPolicy::Prompt);
        assert_eq!(policy.prompt_timeout, Duration::from_secs(120));
    }

    #[test]
    fn input_requests_read_as_the_requests_a_legacy_server_sends() {
        let kind = |value: Value| input_request_kind(&value);
        assert_eq!(
            kind(json!({"method": "roots/list"})),
            Ok(ServerRequestKind::ListRoots)
        );
        let sampling = json!({"messages": [], "maxTokens": 8});
        assert_eq!(
            kind(json!({"method": "sampling/createMessage", "params": sampling.clone()})),
            Ok(ServerRequestKind::Sampling(sampling))
        );
        let schema = json!({"type": "object", "properties": {}});
        assert_eq!(
            kind(json!({"method": "elicitation/create", "params": {
                "message": "name?", "requestedSchema": schema.clone()
            }})),
            Ok(ServerRequestKind::Elicitation {
                message: "name?".into(),
                mode: ElicitationMode::Form { schema },
            })
        );
        assert_eq!(
            kind(json!({"method": "elicitation/create", "params": {
                "mode": "url", "message": "verify", "url": "https://example.com"
            }})),
            Ok(ServerRequestKind::Elicitation {
                message: "verify".into(),
                mode: ElicitationMode::Url {
                    url: "https://example.com".into(),
                    elicitation_id: None,
                },
            }),
            "no elicitationId is needed"
        );
        assert!(kind(json!({"method": "ping"})).is_err());
        assert!(kind(json!({"method": "elicitation/create", "params": {"mode": "url"}})).is_err());
        for url in [
            "file:///etc/passwd",
            "x-apple.systempreferences:",
            "javascript:alert(1)",
            "not a url",
        ] {
            let err = kind(
                json!({"method": "elicitation/create", "params": {"mode": "url", "url": url}}),
            )
            .unwrap_err();
            assert!(err.contains("open"), "{url}: {err}");
        }
    }

    #[tokio::test]
    async fn answers_follow_the_policy_without_a_listener() {
        let client =
            |policy| CocoClient::new(EventSink::new(16), policy, Implementation::new("t", "0"));
        let never = CancellationToken::new();
        let refusing = client(ServerRequestPolicy {
            sampling: SamplingPolicy::Reject,
            elicitation: ElicitationPolicy::Decline,
            roots: RootsPolicy::Fixed {
                roots: vec![Root {
                    uri: "file:///work".into(),
                    name: None,
                }],
            },
            ..ServerRequestPolicy::default()
        });
        let sampled = refusing
            .answer(Value::Null, ServerRequestKind::Sampling(json!({})), &never)
            .await;
        assert!(sampled.is_err());
        let elicited = ServerRequestKind::Elicitation {
            message: "m".into(),
            mode: ElicitationMode::Form { schema: json!({}) },
        };
        assert_eq!(
            refusing.answer(Value::Null, elicited.clone(), &never).await,
            Ok(json!({"action": "decline"}))
        );
        assert_eq!(
            refusing
                .answer(Value::Null, ServerRequestKind::ListRoots, &never)
                .await,
            Ok(json!({"roots": [{"uri": "file:///work"}]}))
        );

        // Asking with nobody listening falls back at once.
        let asking = client(ServerRequestPolicy::default());
        assert_eq!(
            asking.answer(Value::Null, elicited, &never).await,
            Ok(json!({"action": "decline"}))
        );
        assert_eq!(
            asking
                .answer(Value::Null, ServerRequestKind::ListRoots, &never)
                .await,
            Ok(json!({"roots": []}))
        );
        assert_eq!(
            serde_json::to_value(ElicitationAction::Accept).unwrap(),
            json!("accept")
        );
    }
}
