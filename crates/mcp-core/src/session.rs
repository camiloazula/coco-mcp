//! A live connection to one MCP server.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rmcp::model::{
    ArgumentInfo, CallToolRequest, CallToolRequestParams, CancelledNotificationParam,
    ClientRequest, CompleteRequest, CompleteRequestParams, GetPromptRequest,
    GetPromptRequestParams, Implementation, InputRequiredResult, InputResponses,
    ListPromptsRequest, ListResourceTemplatesRequest, ListResourcesRequest, ListToolsRequest,
    PaginatedRequestParams, ReadResourceRequest, ReadResourceRequestParams, Reference,
    RequestMetaObject, ServerResult, SubscribeRequestParams, UnsubscribeRequestParams,
};
// Logging is deprecated by SEP-2577 but still what servers implement.
#[allow(deprecated)]
use rmcp::model::{LoggingLevel, SetLevelRequestParams};
use rmcp::service::{
    ClientInitializeError, ClientLifecycleMode, PeerRequestOptions, RunningService,
    serve_client_with_lifecycle_and_ct,
};
use rmcp::transport::{IntoTransport, Transport};
use rmcp::{Peer, RoleClient};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use time::OffsetDateTime;
use tokio::sync::{broadcast, watch};
use tokio_util::sync::CancellationToken;

use crate::event::{ConnectionState, Event, EventKind, EventSink, ListKind};
use crate::handler::{CocoClient, ServerRequestPolicy};
use crate::lifecycle::{self, Era};
use crate::listen::{self, Listener};
use crate::snapshot::{ListFailure, Snapshot, list_method};
use crate::spec::{ProtocolMode, ServerSpec};
use crate::transport::{self, Heard, TracedTransport, TransportOptions};
use crate::{Error, Result};

/// The levels `logging/setLevel` accepts, least severe first.
pub const LOG_LEVELS: [&str; 8] = [
    "debug",
    "info",
    "notice",
    "warning",
    "error",
    "critical",
    "alert",
    "emergency",
];

/// Rounds of `input_required` one request goes through before it is given up.
pub const MAX_INPUT_ROUNDS: usize = 10;

/// Pages a list is read through before it is given up as endless: a server
/// that always answers with a `nextCursor` would otherwise keep a snapshot
/// from finishing and grow it without bound.
pub const MAX_LIST_PAGES: usize = 1_000;

/// Tunables for [`Session::connect`].
#[derive(Debug, Clone)]
pub struct SessionOptions {
    /// How server-initiated requests are answered.
    pub policy: ServerRequestPolicy,
    /// Unread events kept per subscriber before it starts lagging.
    pub event_capacity: usize,
    /// Name reported to the server.
    pub client_name: String,
    /// Version reported to the server.
    pub client_version: String,
    /// Which protocol era to connect in.
    pub protocol: ProtocolMode,
    /// Upper bound for a single request; `None` waits forever.
    pub request_timeout: Option<Duration>,
    /// How long the server may stay silent, with nothing of ours in hand,
    /// before a `ping` checks it is still there. A ping unanswered for as
    /// long again fails the session. `None` never pings. A modern session
    /// never pings either: 2026-07-28 has no `ping`.
    pub keepalive: Option<Duration>,
    /// Transport-level options (auth header for HTTP).
    pub transport: TransportOptions,
    /// Pre-built sink so callers can subscribe before `connect` and see the
    /// `Connecting` state and how the session started. `None` creates one
    /// with `event_capacity`.
    pub sink: Option<EventSink>,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            policy: ServerRequestPolicy::default(),
            event_capacity: 4096,
            client_name: "coco-mcp".into(),
            client_version: env!("CARGO_PKG_VERSION").into(),
            protocol: ProtocolMode::default(),
            request_timeout: Some(Duration::from_secs(60)),
            keepalive: Some(Duration::from_secs(30)),
            transport: TransportOptions::default(),
            sink: None,
        }
    }
}

/// A transport ready to start a session on, and the task forwarding its
/// server's stderr.
struct Opened<T> {
    transport: T,
    stderr: Option<tokio::task::JoinHandle<()>>,
}

/// A session `rmcp` started, before it is wrapped in [`Session`].
struct Started {
    service: RunningService<RoleClient, CocoClient>,
    heard: Heard,
    stderr: Option<tokio::task::JoinHandle<()>>,
}

/// One page of a list: its items and the cursor of the next page, if any.
type Page<T> = (Vec<T>, Option<String>);

/// Why a start failed, and the stderr task that may still say more.
struct NotStarted {
    error: ClientInitializeError,
    stderr: Option<tokio::task::JoinHandle<()>>,
}

/// Follows and stops one request while it runs. [`Self::cancel`] tells the
/// server with `notifications/cancelled` and ends the call with
/// [`Error::Cancelled`]; [`Self::progress_token`] names the
/// `notifications/progress` that belong to it.
#[derive(Debug, Clone, Default)]
pub struct RequestControl {
    cancel: CancellationToken,
    progress_token: Arc<Mutex<Option<Value>>>,
}

impl RequestControl {
    /// A control for one new request.
    pub fn new() -> Self {
        Self::default()
    }

    /// Stop the request. Does nothing once it has finished.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// Whether [`Self::cancel`] was called.
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// The progress token the request went out with, as it appears in
    /// [`EventKind::Progress`], once it has been sent. A request that went
    /// through rounds of input carries a new token each round; this is the
    /// latest.
    pub fn progress_token(&self) -> Option<Value> {
        self.progress_token
            .lock()
            .ok()
            .and_then(|token| token.clone())
    }

    fn set_progress_token(&self, token: Value) {
        if let Ok(mut slot) = self.progress_token.lock() {
            *slot = Some(token);
        }
    }
}

/// What a `completion/complete` request completes an argument of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompletionTarget {
    /// An argument of the prompt with this name.
    Prompt(String),
    /// A variable of the resource template with this URI template.
    ResourceTemplate(String),
}

/// Suggestions from `completion/complete`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Completion {
    /// Suggested values, in the server's order.
    pub values: Vec<String>,
    /// How many values there are in all, if the server said.
    pub total: Option<u32>,
    /// Whether the server has more values than it sent.
    pub has_more: bool,
}

/// Result of `tools/call`.
#[derive(Debug, Clone, PartialEq)]
pub struct CallOutcome {
    /// Raw `CallToolResult` JSON.
    pub raw: Value,
    /// `content` blocks.
    pub content: Vec<Value>,
    /// `isError` flag (absent means `false`).
    pub is_error: bool,
    /// Round-trip time.
    pub elapsed: Duration,
}

/// Result of `resources/read`.
#[derive(Debug, Clone, PartialEq)]
pub struct ResourceOutcome {
    /// Raw `ReadResourceResult` JSON.
    pub raw: Value,
    /// `contents` entries (text or blob).
    pub contents: Vec<Value>,
    /// Round-trip time.
    pub elapsed: Duration,
}

/// Result of `prompts/get`.
#[derive(Debug, Clone, PartialEq)]
pub struct PromptOutcome {
    /// Raw `GetPromptResult` JSON.
    pub raw: Value,
    /// Round-trip time.
    pub elapsed: Duration,
}

#[derive(Debug)]
struct Inner {
    spec: ServerSpec,
    peer: Peer<RoleClient>,
    sink: EventSink,
    state: watch::Sender<ConnectionState>,
    cancel: tokio_util::sync::CancellationToken,
    era: Era,
    /// Answers the input requests of a round trip, as it answers a legacy
    /// server's own requests.
    handler: CocoClient,
    /// The log level a modern session sends with every request.
    log_level: Mutex<Option<String>>,
    /// The `subscriptions/listen` stream of a modern session.
    listener: Option<Listener>,
    request_timeout: Option<Duration>,
    /// The transport's record of what the server still owes an answer to,
    /// so a request given up on is struck from it (see `send`).
    heard: Heard,
    // Kept so the tasks are aborted when the last handle drops.
    _stderr_task: Option<tokio::task::JoinHandle<()>>,
    _wait_task: tokio::task::JoinHandle<()>,
    _keepalive_task: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(task) = &self._stderr_task {
            task.abort();
        }
        if let Some(task) = &self._keepalive_task {
            task.abort();
        }
    }
}

/// How the lists of one snapshot went so far.
#[derive(Debug, Default)]
struct Listing {
    failures: Vec<ListFailure>,
    /// The request timeout a list ran out of; no list is requested after it.
    timed_out: Option<Duration>,
}

/// Cloneable handle to a connected server. Dropping the last clone closes it.
#[derive(Debug, Clone)]
pub struct Session {
    inner: Arc<Inner>,
}

impl Session {
    /// Connect according to `spec`, in the era `options.protocol` asks for.
    pub async fn connect(spec: ServerSpec, options: SessionOptions) -> Result<Self> {
        transport::check_spec(&spec)?;
        let sink = options
            .sink
            .clone()
            .unwrap_or_else(|| EventSink::new(options.event_capacity));
        match spec.clone() {
            ServerSpec::Stdio {
                command,
                args,
                env,
                cwd,
            } => {
                let stderr_sink = sink.clone();
                let open = move || {
                    Some(
                        transport::spawn_stdio(&command, &args, &env, cwd.as_deref()).map(
                            |(transport, stderr)| Opened {
                                transport,
                                stderr: stderr
                                    .map(|s| transport::forward_stderr(s, stderr_sink.clone())),
                            },
                        ),
                    )
                };
                Self::connect_inner(spec, open, options, sink).await
            }
            ServerSpec::Http { url, headers, .. } => {
                let transport_options = options.transport.clone();
                let open = move || {
                    Some(
                        transport::build_http(&url, &headers, &transport_options).map(
                            |transport| Opened {
                                transport,
                                stderr: None,
                            },
                        ),
                    )
                };
                Self::connect_inner(spec, open, options, sink).await
            }
        }
    }

    /// Connect over an already-built transport (tests use an in-process
    /// duplex). An Auto connect that needs a second transport for the
    /// handshake fails here; see [`Self::connect_with_transports`].
    pub async fn connect_with_transport<T, E, A>(
        spec: ServerSpec,
        transport: T,
        options: SessionOptions,
    ) -> Result<Self>
    where
        T: IntoTransport<RoleClient, E, A>,
        E: std::error::Error + Send + Sync + 'static,
    {
        let mut transport = Some(transport);
        Self::connect_with_transports(spec, move || transport.take(), options).await
    }

    /// Connect over transports `open` builds: the first, and a second when an
    /// Auto connect falls back to the handshake after `server/discover`
    /// found no shared version. `open` returns `None` when it has no more.
    pub async fn connect_with_transports<F, T, E, A>(
        spec: ServerSpec,
        mut open: F,
        options: SessionOptions,
    ) -> Result<Self>
    where
        F: FnMut() -> Option<T>,
        T: IntoTransport<RoleClient, E, A>,
        E: std::error::Error + Send + Sync + 'static,
    {
        let sink = options
            .sink
            .clone()
            .unwrap_or_else(|| EventSink::new(options.event_capacity));
        let open = move || {
            open().map(|t| {
                Ok(Opened {
                    transport: t.into_transport(),
                    stderr: None,
                })
            })
        };
        Self::connect_inner(spec, open, options, sink).await
    }

    async fn connect_inner<F, T>(
        spec: ServerSpec,
        mut open: F,
        options: SessionOptions,
        sink: EventSink,
    ) -> Result<Self>
    where
        F: FnMut() -> Option<Result<Opened<T>>>,
        T: Transport<RoleClient> + 'static,
    {
        let opened =
            open().ok_or_else(|| Error::Transport("no transport to connect over".into()))??;
        let (state, _) = watch::channel(ConnectionState::Connecting);
        sink.emit(EventKind::StateChange {
            state: ConnectionState::Connecting,
            detail: Some(spec.label()),
        });
        let mode = options.protocol;
        let handler = CocoClient::new(
            sink.clone(),
            options.policy.clone(),
            Implementation::new(options.client_name.clone(), options.client_version.clone()),
        );
        let cancel = tokio_util::sync::CancellationToken::new();
        let mut started = start(
            handler.clone(),
            opened,
            lifecycle::lifecycle(mode),
            &sink,
            cancel.clone(),
        )
        .await;
        if mode == ProtocolMode::Auto
            && let Err(failed) = &started
            && lifecycle::falls_back(&failed.error)
            && let Some(reopened) = open()
        {
            let why = lifecycle::describe(&failed.error, mode);
            match reopened {
                Ok(opened) => {
                    sink.emit(EventKind::StateChange {
                        state: ConnectionState::Connecting,
                        detail: Some(format!("{why}; connecting with the initialize handshake")),
                    });
                    started = start(
                        handler.clone(),
                        opened,
                        ClientLifecycleMode::Initialize,
                        &sink,
                        cancel.clone(),
                    )
                    .await;
                }
                Err(e) => {
                    let detail = format!("{why}; the handshake could not start: {e}");
                    state.send_replace(ConnectionState::Failed);
                    sink.emit(EventKind::StateChange {
                        state: ConnectionState::Failed,
                        detail: Some(detail.clone()),
                    });
                    return Err(Error::Initialize(detail));
                }
            }
        }
        let Started {
            service,
            heard,
            stderr: stderr_task,
        } = match started {
            Ok(started) => started,
            Err(failed) => {
                let NotStarted { error, stderr } = *failed;
                let detail = lifecycle::describe(&error, mode);
                let auth = error
                    .is_authorization_required()
                    .then(|| error.auth_challenge().map(str::to_owned));
                state.send_replace(ConnectionState::Failed);
                sink.emit(EventKind::StateChange {
                    state: ConnectionState::Failed,
                    detail: Some(detail.clone()),
                });
                if let Some(task) = stderr {
                    // Give the child a moment to flush its stderr so the
                    // reason for the failure reaches the log.
                    let _ = tokio::time::timeout(Duration::from_millis(200), task).await;
                }
                // A transport that never carried the first request is a
                // server that could not be reached, not one that answered
                // the start badly or went away during it.
                return Err(match auth {
                    Some(challenge) => Error::AuthRequired { challenge },
                    None if lifecycle::reached(&error) => Error::Initialize(detail),
                    None => Error::Transport(detail),
                });
            }
        };
        let peer = service.peer().clone();
        let era = peer
            .peer_info()
            .map_or(Era::Legacy, |info| Era::of(info.protocol_version.as_str()));
        // A modern server announces changes only on a stream the client opens.
        let listener = (era == Era::Modern).then(|| {
            let capabilities = peer
                .peer_info()
                .and_then(|info| serde_json::to_value(&info.capabilities).ok())
                .unwrap_or(Value::Null);
            Listener::spawn(
                peer.clone(),
                listen::list_filter(&capabilities),
                sink.clone(),
                cancel.clone(),
            )
        });
        state.send_replace(ConnectionState::Connected);
        sink.emit(EventKind::StateChange {
            state: ConnectionState::Connected,
            detail: None,
        });

        let wait_task = {
            let sink = sink.clone();
            let state = state.clone();
            tokio::spawn(async move {
                let (next, detail) = match service.waiting().await {
                    Ok(reason) => (ConnectionState::Disconnected, format!("{reason:?}")),
                    Err(e) => (ConnectionState::Failed, e.to_string()),
                };
                // A keepalive that gave up has already said why the session
                // ended; closing the transport afterwards is not news.
                if *state.borrow() == ConnectionState::Failed {
                    return;
                }
                state.send_replace(next);
                sink.emit(EventKind::StateChange {
                    state: next,
                    detail: Some(detail),
                });
            })
        };

        // 2026-07-28 has no `ping`; a stdio server that exits still ends the
        // session through the wait task.
        let keepalive = options.keepalive.filter(|_| era == Era::Legacy);
        let keepalive_task = keepalive.map(|interval| {
            tokio::spawn(keepalive_loop(
                peer.clone(),
                heard.clone(),
                interval,
                state.clone(),
                sink.clone(),
                cancel.clone(),
            ))
        });

        Ok(Self {
            inner: Arc::new(Inner {
                spec,
                peer,
                sink,
                state,
                cancel,
                era,
                handler,
                log_level: Mutex::new(None),
                listener,
                request_timeout: options.request_timeout,
                heard,
                _stderr_task: stderr_task,
                _wait_task: wait_task,
                _keepalive_task: keepalive_task,
            }),
        })
    }

    /// The spec this session was opened with.
    pub fn spec(&self) -> &ServerSpec {
        &self.inner.spec
    }

    /// The era of the protocol version the session agreed on.
    pub fn era(&self) -> Era {
        self.inner.era
    }

    /// Subscribe to the event log.
    pub fn events(&self) -> broadcast::Receiver<Event> {
        self.inner.sink.subscribe()
    }

    /// Current connection state.
    pub fn state(&self) -> ConnectionState {
        *self.inner.state.borrow()
    }

    /// Watch connection state changes.
    pub fn state_watch(&self) -> watch::Receiver<ConnectionState> {
        self.inner.state.subscribe()
    }

    /// What the server said about itself as JSON (`protocolVersion`,
    /// `capabilities`, `serverInfo`, `instructions`), from the `initialize`
    /// result or from `server/discover`, in the same shape.
    pub fn server_info(&self) -> Option<Value> {
        self.inner
            .peer
            .peer_info()
            .and_then(|info| serde_json::to_value(&*info).ok())
    }

    fn capability(&self, name: &str) -> bool {
        self.server_info()
            .and_then(|v| v.get("capabilities").and_then(|c| c.get(name)).cloned())
            .is_some_and(|v| !v.is_null())
    }

    async fn timed<F, T>(&self, fut: F) -> Result<T>
    where
        F: Future<Output = std::result::Result<T, rmcp::ServiceError>>,
    {
        match self.inner.request_timeout {
            Some(limit) => match tokio::time::timeout(limit, fut).await {
                Ok(res) => res.map_err(Error::from),
                Err(_) => Err(Error::Timeout(limit)),
            },
            None => fut.await.map_err(Error::from),
        }
    }

    /// What every request of this session carries besides its params: on a
    /// modern session, the log level last chosen, which 2026-07-28 sends in
    /// each request's `_meta` rather than with `logging/setLevel`.
    #[allow(deprecated)]
    fn request_options(&self) -> PeerRequestOptions {
        let level = self
            .inner
            .log_level
            .lock()
            .ok()
            .and_then(|level| level.clone())
            .filter(|_| self.inner.era == Era::Modern)
            .and_then(|level| serde_json::from_value::<LoggingLevel>(Value::String(level)).ok());
        match level {
            Some(level) => {
                let mut meta = RequestMetaObject::new();
                meta.set_log_level(level);
                PeerRequestOptions::no_options().with_meta(meta)
            }
            None => PeerRequestOptions::no_options(),
        }
    }

    /// Send one request and wait for its answer. Cancelling `control`, or
    /// running past the request timeout, tells the server with
    /// `notifications/cancelled` rather than leaving it working on a request
    /// nobody waits for.
    async fn send(&self, request: ClientRequest, control: &RequestControl) -> Result<ServerResult> {
        if control.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let peer = &self.inner.peer;
        let handle = peer
            .send_cancellable_request(request, self.request_options())
            .await?;
        control.set_progress_token(
            serde_json::to_value(&handle.progress_token).unwrap_or(Value::Null),
        );
        let id = handle.id.clone();
        let limit = self.inner.request_timeout;
        let expired = async {
            match limit {
                Some(limit) => tokio::time::sleep(limit).await,
                None => std::future::pending().await,
            }
        };
        let (error, reason) = tokio::select! {
            answer = handle.await_response() => return answer.map_err(Error::from),
            () = control.cancel.cancelled() => (Error::Cancelled, "cancelled by the client"),
            () = expired => (Error::Timeout(limit.unwrap_or_default()), "request timed out"),
        };
        // The server may still answer, but nobody waits: without this the
        // keepalive would take the open entry as work in hand and never ping.
        if let Ok(id) = serde_json::to_value(&id) {
            self.inner.heard.forget(&id.to_string());
        }
        let _ = peer
            .notify_cancelled(CancelledNotificationParam::new(
                Some(id),
                Some(reason.to_owned()),
            ))
            .await;
        Err(error)
    }

    /// Send the request `make` builds and wait for the result `pick` takes
    /// out of the answer. A 2026-07-28 server may answer `input_required`
    /// instead: each input request it names is answered through the handler,
    /// as a legacy server's own request is, and the request is sent again,
    /// with a new id, the answers and the server's state echoed as it sent
    /// it. After [`MAX_INPUT_ROUNDS`] rounds the request is given up. The
    /// request timeout applies to each round, not to the time a person takes
    /// to answer.
    async fn request<T>(
        &self,
        make: impl Fn(Option<InputResponses>, Option<String>) -> ClientRequest,
        control: &RequestControl,
        pick: fn(ServerResult) -> Option<T>,
    ) -> Result<T> {
        let (mut responses, mut state) = (None, None);
        for round in 0..MAX_INPUT_ROUNDS {
            match self
                .send(make(responses.take(), state.take()), control)
                .await?
            {
                ServerResult::InputRequiredResult(asked) => {
                    (responses, state) = self.fulfil(asked, round, control).await?;
                }
                answer => {
                    return pick(answer)
                        .ok_or_else(|| Error::Transport("unexpected response".into()));
                }
            }
        }
        Err(Error::InputRounds(MAX_INPUT_ROUNDS))
    }

    /// The answers to one `input_required` result and the state to echo.
    async fn fulfil(
        &self,
        asked: InputRequiredResult,
        round: usize,
        control: &RequestControl,
    ) -> Result<(Option<InputResponses>, Option<String>)> {
        let requests = asked.input_requests.unwrap_or_default();
        if requests.is_empty() {
            if asked.request_state.is_none() {
                return Err(Error::Transport(
                    "the server asked for input without saying what".into(),
                ));
            }
            // Only state to carry: the server is not done yet. Wait a little,
            // longer each round, before asking again.
            let wait = Duration::from_millis((50u64 << round.min(3)).min(250));
            tokio::select! {
                () = tokio::time::sleep(wait) => {}
                () = control.cancel.cancelled() => return Err(Error::Cancelled),
            }
            return Ok((None, asked.request_state));
        }
        let client = &self.inner.handler;
        let answers =
            futures::future::try_join_all(requests.into_iter().map(|(key, request)| async move {
                let raw = serde_json::to_value(&request)?;
                let kind = crate::handler::input_request_kind(&raw).map_err(Error::Refused)?;
                let answer = client
                    .answer(Value::String(key.clone()), kind, &control.cancel)
                    .await
                    .map_err(|e| Error::Refused(e.message.into_owned()))?;
                Ok::<_, Error>((key, answer))
            }))
            .await?;
        if control.is_cancelled() {
            return Err(Error::Cancelled);
        }
        Ok((Some(answers.into_iter().collect()), asked.request_state))
    }

    /// Every page of one list, asked for directly rather than through
    /// `rmcp`'s list helpers, which answer from a cache and hand back a stale
    /// copy when the server fails: a debugger shows what the server says
    /// now.
    async fn list_all<T>(
        &self,
        request: fn(PaginatedRequestParams) -> ClientRequest,
        page: fn(ServerResult) -> Option<Page<T>>,
    ) -> Result<Vec<T>> {
        let mut items = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_LIST_PAGES {
            let params = PaginatedRequestParams::default().with_cursor(cursor.clone());
            let answer = self.send(request(params), &RequestControl::new()).await?;
            let (more, next) =
                page(answer).ok_or_else(|| Error::Transport("unexpected response".into()))?;
            items.extend(more);
            match next {
                Some(next) if cursor.as_deref() == Some(next.as_str()) => {
                    return Err(Error::Transport(
                        "the server answered a page with its own cursor".into(),
                    ));
                }
                Some(next) => cursor = Some(next),
                None => return Ok(items),
            }
        }
        Err(Error::Transport(format!(
            "the list did not end after {MAX_LIST_PAGES} pages"
        )))
    }

    async fn list_tools(&self) -> Result<Vec<rmcp::model::Tool>> {
        self.list_all(
            |params| ClientRequest::ListToolsRequest(ListToolsRequest::with_param(params)),
            |answer| match answer {
                ServerResult::ListToolsResult(r) => Some((r.tools, r.next_cursor)),
                _ => None,
            },
        )
        .await
    }

    async fn list_resources(&self) -> Result<Vec<rmcp::model::Resource>> {
        self.list_all(
            |params| ClientRequest::ListResourcesRequest(ListResourcesRequest::with_param(params)),
            |answer| match answer {
                ServerResult::ListResourcesResult(r) => Some((r.resources, r.next_cursor)),
                _ => None,
            },
        )
        .await
    }

    async fn list_resource_templates(&self) -> Result<Vec<rmcp::model::ResourceTemplate>> {
        self.list_all(
            |params| {
                ClientRequest::ListResourceTemplatesRequest(
                    ListResourceTemplatesRequest::with_param(params),
                )
            },
            |answer| match answer {
                ServerResult::ListResourceTemplatesResult(r) => {
                    Some((r.resource_templates, r.next_cursor))
                }
                _ => None,
            },
        )
        .await
    }

    async fn list_prompts(&self) -> Result<Vec<rmcp::model::Prompt>> {
        self.list_all(
            |params| ClientRequest::ListPromptsRequest(ListPromptsRequest::with_param(params)),
            |answer| match answer {
                ServerResult::ListPromptsResult(r) => Some((r.prompts, r.next_cursor)),
                _ => None,
            },
        )
        .await
    }

    /// One list of a snapshot, best effort. A server that answers
    /// method-not-found does not implement the list, which is an empty list
    /// rather than a fault. Any other error is recorded in `listing` and
    /// leaves the list empty, so one broken list does not hide the others.
    /// Only a closed connection fails, since nothing further can be listed.
    ///
    /// After a list times out, `fut` is dropped unpolled, so the request is
    /// never sent, and the list is recorded as not requested: a server that
    /// stopped answering would otherwise hold the snapshot for one full
    /// timeout per list.
    async fn listed<F, T>(
        &self,
        method: &'static str,
        fut: F,
        listing: &mut Listing,
    ) -> Result<Vec<T>>
    where
        F: Future<Output = Result<Vec<T>>>,
    {
        if let Some(limit) = listing.timed_out {
            listing.failures.push(ListFailure {
                method: method.to_owned(),
                error: format!("not requested: an earlier list timed out after {limit:?}"),
            });
            return Ok(Vec::new());
        }
        match fut.await {
            Ok(items) => Ok(items),
            Err(Error::Server { code, .. })
                if code == i64::from(rmcp::model::ErrorCode::METHOD_NOT_FOUND.0) =>
            {
                Ok(Vec::new())
            }
            Err(Error::Closed) => Err(Error::Closed),
            Err(e) => {
                if let Error::Timeout(limit) = e {
                    listing.timed_out = Some(limit);
                }
                listing.failures.push(ListFailure {
                    method: method.to_owned(),
                    error: e.to_string(),
                });
                Ok(Vec::new())
            }
        }
    }

    /// List everything the server advertises. Lists whose capability the
    /// server did not declare are left empty rather than queried. A list
    /// that fails is left empty too and named in
    /// [`Snapshot::list_failures`]. Once a list times out, the lists after it
    /// are named there without being requested, so a server that stopped
    /// answering costs one timeout rather than one per list. The snapshot
    /// itself fails only when the server never said what it is or the
    /// connection closed.
    pub async fn snapshot(&self) -> Result<Snapshot> {
        let info = self
            .server_info()
            .ok_or_else(|| Error::Transport("no server info".into()))?;
        let mut listing = Listing::default();
        let tools = if self.capability("tools") {
            self.listed(list_method::TOOLS, self.list_tools(), &mut listing)
                .await?
        } else {
            Vec::new()
        };
        let (resources, resource_templates) = if self.capability("resources") {
            (
                self.listed(list_method::RESOURCES, self.list_resources(), &mut listing)
                    .await?,
                self.listed(
                    list_method::RESOURCE_TEMPLATES,
                    self.list_resource_templates(),
                    &mut listing,
                )
                .await?,
            )
        } else {
            (Vec::new(), Vec::new())
        };
        let prompts = if self.capability("prompts") {
            self.listed(list_method::PROMPTS, self.list_prompts(), &mut listing)
                .await?
        } else {
            Vec::new()
        };
        let convert =
            |v: Value| -> Result<Snapshot> { serde_json::from_value(v).map_err(Error::from) };
        convert(serde_json::json!({
            "protocolVersion": info.get("protocolVersion").cloned().unwrap_or(Value::Null),
            "serverInfo": info.get("serverInfo").cloned().unwrap_or(Value::Null),
            "capabilities": info.get("capabilities").cloned().unwrap_or(Value::Null),
            "instructions": info.get("instructions").cloned().unwrap_or(Value::Null),
            "tools": tools,
            "resources": resources,
            "resourceTemplates": resource_templates,
            "prompts": prompts,
            "listFailures": listing.failures,
            "takenAt": OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339)
                .map_err(|e| Error::Transport(e.to_string()))?,
        }))
    }

    /// Read the lists of `kind` again, after the server announced that they
    /// changed, and return `snapshot` with them replaced. The other lists,
    /// the `initialize` result and the failures of other lists are kept; a
    /// list that fails now is left empty and named, as in [`Self::snapshot`].
    pub async fn relist(&self, snapshot: &Snapshot, kind: ListKind) -> Result<Snapshot> {
        let mut listing = Listing::default();
        let mut next = snapshot.clone();
        let methods: &[&str] = match kind {
            ListKind::Tools => &[list_method::TOOLS],
            ListKind::Resources => &[list_method::RESOURCES, list_method::RESOURCE_TEMPLATES],
            ListKind::Prompts => &[list_method::PROMPTS],
        };
        next.list_failures
            .retain(|f| !methods.contains(&f.method.as_str()));
        match kind {
            ListKind::Tools if self.capability("tools") => {
                let tools = self
                    .listed(list_method::TOOLS, self.list_tools(), &mut listing)
                    .await?;
                next.tools = converted(tools)?;
            }
            ListKind::Resources if self.capability("resources") => {
                let resources = self
                    .listed(list_method::RESOURCES, self.list_resources(), &mut listing)
                    .await?;
                let templates = self
                    .listed(
                        list_method::RESOURCE_TEMPLATES,
                        self.list_resource_templates(),
                        &mut listing,
                    )
                    .await?;
                next.resources = converted(resources)?;
                next.resource_templates = converted(templates)?;
            }
            ListKind::Prompts if self.capability("prompts") => {
                let prompts = self
                    .listed(list_method::PROMPTS, self.list_prompts(), &mut listing)
                    .await?;
                next.prompts = converted(prompts)?;
            }
            _ => {}
        }
        next.list_failures.extend(listing.failures);
        next.taken_at = OffsetDateTime::now_utc();
        Ok(next)
    }

    /// Call a tool. `args` must be a JSON object (or `null` for no arguments).
    pub async fn call_tool(&self, name: &str, args: Value) -> Result<CallOutcome> {
        self.call_tool_with(name, args, &RequestControl::new())
            .await
    }

    /// [`Self::call_tool`], followed and stopped through `control`.
    pub async fn call_tool_with(
        &self,
        name: &str,
        args: Value,
        control: &RequestControl,
    ) -> Result<CallOutcome> {
        let arguments = match args {
            Value::Null => None,
            Value::Object(map) => Some(map),
            other => {
                return Err(Error::InvalidArguments(format!(
                    "expected a JSON object, got {}",
                    type_name(&other)
                )));
            }
        };
        let mut params = CallToolRequestParams::new(name.to_owned());
        params.arguments = arguments;
        let started = Instant::now();
        let make = |responses, state| {
            let mut params = params.clone();
            params.input_responses = responses;
            params.request_state = state;
            ClientRequest::CallToolRequest(CallToolRequest::new(params))
        };
        let result = self
            .request(make, control, |answer| match answer {
                ServerResult::CallToolResult(result) => Some(result),
                _ => None,
            })
            .await?;
        let elapsed = started.elapsed();
        let raw = serde_json::to_value(&result)?;
        Ok(CallOutcome {
            content: raw
                .get("content")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
            is_error: raw.get("isError").and_then(Value::as_bool).unwrap_or(false),
            elapsed,
            raw,
        })
    }

    /// Read a resource by URI. Always asks the server, so a subscribed
    /// resource that changed reads as it is now.
    pub async fn read_resource(&self, uri: &str) -> Result<ResourceOutcome> {
        self.read_resource_with(uri, &RequestControl::new()).await
    }

    /// [`Self::read_resource`], followed and stopped through `control`.
    pub async fn read_resource_with(
        &self,
        uri: &str,
        control: &RequestControl,
    ) -> Result<ResourceOutcome> {
        let started = Instant::now();
        let make = |responses, state| {
            let mut params = ReadResourceRequestParams::new(uri.to_owned());
            params.input_responses = responses;
            params.request_state = state;
            ClientRequest::ReadResourceRequest(ReadResourceRequest::new(params))
        };
        let result = self
            .request(make, control, |answer| match answer {
                ServerResult::ReadResourceResult(result) => Some(result),
                _ => None,
            })
            .await?;
        let elapsed = started.elapsed();
        let raw = serde_json::to_value(&result)?;
        Ok(ResourceOutcome {
            contents: raw
                .get("contents")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
            elapsed,
            raw,
        })
    }

    /// Get a prompt with string arguments.
    pub async fn get_prompt(&self, name: &str, args: Map<String, Value>) -> Result<PromptOutcome> {
        self.get_prompt_with(name, args, &RequestControl::new())
            .await
    }

    /// [`Self::get_prompt`], followed and stopped through `control`.
    pub async fn get_prompt_with(
        &self,
        name: &str,
        args: Map<String, Value>,
        control: &RequestControl,
    ) -> Result<PromptOutcome> {
        let mut params = GetPromptRequestParams::new(name.to_owned());
        if !args.is_empty() {
            params.arguments = Some(args);
        }
        let started = Instant::now();
        let make = |responses, state| {
            let mut params = params.clone();
            params.input_responses = responses;
            params.request_state = state;
            ClientRequest::GetPromptRequest(GetPromptRequest::new(params))
        };
        let result = self
            .request(make, control, |answer| match answer {
                ServerResult::GetPromptResult(result) => Some(result),
                _ => None,
            })
            .await?;
        let elapsed = started.elapsed();
        let raw = serde_json::to_value(&result)?;
        Ok(PromptOutcome { elapsed, raw })
    }

    /// Subscribe to changes of `uri`: `resources/subscribe` on a legacy
    /// session; on a modern one, the `subscriptions/listen` stream opened
    /// again with `uri` in it, which fails when the server leaves it out.
    /// Changes arrive as [`EventKind::ResourceUpdated`] either way.
    #[allow(deprecated)]
    pub async fn subscribe_resource(&self, uri: &str) -> Result<()> {
        if let Some(listener) = &self.inner.listener {
            return listener.subscribe(uri).await;
        }
        self.timed(
            self.inner
                .peer
                .subscribe(SubscribeRequestParams::new(uri.to_owned())),
        )
        .await
    }

    /// Cancel a resource subscription (see [`Self::subscribe_resource`]).
    #[allow(deprecated)]
    pub async fn unsubscribe_resource(&self, uri: &str) -> Result<()> {
        if let Some(listener) = &self.inner.listener {
            return listener.unsubscribe(uri).await;
        }
        self.timed(
            self.inner
                .peer
                .unsubscribe(UnsubscribeRequestParams::new(uri.to_owned())),
        )
        .await
    }

    /// Ask the server to send log messages at `level` and above. `level` is
    /// one of [`LOG_LEVELS`]. A legacy session sends `logging/setLevel`. A
    /// modern one sends nothing now: it keeps the level and puts it in the
    /// `_meta` of every request after, and its server sends log messages
    /// only while one of them runs.
    #[allow(deprecated)]
    pub async fn set_log_level(&self, level: &str) -> Result<()> {
        let parsed: LoggingLevel = serde_json::from_value(Value::String(level.to_owned()))
            .map_err(|_| Error::InvalidArguments(format!("unknown log level `{level}`")))?;
        if self.inner.era == Era::Modern {
            if let Ok(mut kept) = self.inner.log_level.lock() {
                *kept = Some(level.to_owned());
            }
            return Ok(());
        }
        self.timed(
            self.inner
                .peer
                .set_level(SetLevelRequestParams::new(parsed)),
        )
        .await
    }

    /// Suggestions for `argument` of `target` (`completion/complete`), given
    /// what is typed so far and the arguments already filled in.
    pub async fn complete(
        &self,
        target: &CompletionTarget,
        argument: &str,
        typed: &str,
        filled: &BTreeMap<String, String>,
    ) -> Result<Completion> {
        let reference = match target {
            CompletionTarget::Prompt(name) => Reference::for_prompt(name.clone()),
            CompletionTarget::ResourceTemplate(uri) => Reference::for_resource(uri.clone()),
        };
        let mut params = CompleteRequestParams::new(reference, ArgumentInfo::new(argument, typed));
        if !filled.is_empty() {
            params = params.with_context(serde_json::from_value(
                serde_json::json!({ "arguments": filled }),
            )?);
        }
        let request = ClientRequest::CompleteRequest(CompleteRequest::new(params));
        let result = match self.send(request, &RequestControl::new()).await? {
            ServerResult::CompleteResult(result) => result,
            _ => return Err(Error::Transport("unexpected response".into())),
        };
        Ok(Completion {
            values: result.completion.values,
            total: result.completion.total,
            has_more: result.completion.has_more.unwrap_or(false),
        })
    }

    /// Tell the server the client's roots changed
    /// (`notifications/roots/list_changed`), so it asks for them again. A
    /// modern session sends nothing: 2026-07-28 has no such notification, and
    /// its server receives the roots when it asks during a request.
    #[allow(deprecated)]
    pub async fn notify_roots_changed(&self) -> Result<()> {
        if self.inner.era == Era::Modern {
            return Ok(());
        }
        self.inner
            .peer
            .notify_roots_list_changed()
            .await
            .map_err(Error::from)
    }

    /// Close the connection. Safe to call more than once.
    pub fn close(&self) {
        self.inner.cancel.cancel();
    }
}

/// Start a session over `opened` the way `lifecycle` says, tracing its
/// transport into `sink`.
async fn start<T>(
    handler: CocoClient,
    opened: Opened<T>,
    lifecycle: ClientLifecycleMode,
    sink: &EventSink,
    cancel: CancellationToken,
) -> std::result::Result<Started, Box<NotStarted>>
where
    T: Transport<RoleClient> + 'static,
{
    let traced = TracedTransport::new(opened.transport, sink.clone());
    let heard = traced.heard();
    match serve_client_with_lifecycle_and_ct(handler, traced, lifecycle, cancel).await {
        Ok(service) => Ok(Started {
            service,
            heard,
            stderr: opened.stderr,
        }),
        Err(error) => Err(Box::new(NotStarted {
            error,
            stderr: opened.stderr,
        })),
    }
}

/// Ping a server that has been silent for `interval` while nothing of ours
/// is in its hands, and fail the session when the ping goes unanswered for
/// as long again. A server that went away without closing its transport
/// would otherwise read as connected until the next request failed.
async fn keepalive_loop(
    peer: Peer<RoleClient>,
    heard: Heard,
    interval: Duration,
    state: watch::Sender<ConnectionState>,
    sink: EventSink,
    cancel: CancellationToken,
) {
    loop {
        let quiet = heard.since();
        if quiet < interval || heard.awaiting_server() {
            let wait = interval.saturating_sub(quiet).max(interval / 4);
            tokio::select! {
                () = tokio::time::sleep(wait) => continue,
                () = cancel.cancelled() => return,
            }
        }
        let ping = peer.send_request(ClientRequest::PingRequest(Default::default()));
        let answered = tokio::select! {
            answered = tokio::time::timeout(interval, ping) => answered,
            () = cancel.cancelled() => return,
        };
        match answered {
            // Any answer, even an error, shows the server is there.
            Ok(Ok(_) | Err(rmcp::ServiceError::McpError(_))) => {}
            // The session's wait task reports a closed transport.
            Ok(Err(_)) => return,
            Err(_) => {
                state.send_replace(ConnectionState::Failed);
                sink.emit(EventKind::StateChange {
                    state: ConnectionState::Failed,
                    detail: Some(format!(
                        "the server stopped answering: no reply to ping within {interval:?}"
                    )),
                });
                cancel.cancel();
                return;
            }
        }
    }
}

/// Re-type rmcp's list items as the snapshot's own, through their JSON.
fn converted<T: Serialize, U: DeserializeOwned>(items: Vec<T>) -> Result<Vec<U>> {
    Ok(serde_json::from_value(serde_json::to_value(items)?)?)
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}
