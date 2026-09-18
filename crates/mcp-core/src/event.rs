//! Events emitted by a [`crate::Session`] for the log drawer, the CLI and tests.
//!
//! Every wire message (request, response, notification, error) becomes one
//! event with a timestamp; responses carry the elapsed time since their
//! request. Server-initiated requests (sampling, elicitation, roots) are
//! surfaced as [`EventKind::ServerRequest`] and answered through
//! [`ServerRequest::respond`].

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;
use tokio::sync::{broadcast, oneshot};

/// Which side sent a wire message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Client (Coco MCP) to server.
    Outbound,
    /// Server to client.
    Inbound,
}

/// Lifecycle of a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionState {
    /// The transport is being established and the session is starting,
    /// through the `initialize` handshake or `server/discover`.
    Connecting,
    /// The session started and the protocol version is agreed.
    Connected,
    /// The session ended cleanly (closed by us or by the server).
    Disconnected,
    /// The session ended with an error.
    Failed,
}

/// Which list a `*/list_changed` notification refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ListKind {
    /// `notifications/tools/list_changed`.
    Tools,
    /// `notifications/resources/list_changed`.
    Resources,
    /// `notifications/prompts/list_changed`.
    Prompts,
}

/// Coarse category used by the log drawer's filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventCategory {
    /// JSON-RPC request in either direction.
    Request,
    /// JSON-RPC success response in either direction.
    Response,
    /// JSON-RPC error response in either direction.
    Error,
    /// JSON-RPC notification in either direction.
    Notification,
    /// Connection state changes.
    State,
    /// A server-initiated request awaiting an answer.
    ServerRequest,
    /// A line the server process wrote to stderr.
    Stderr,
}

/// One entry in the event log.
#[derive(Debug, Clone)]
pub struct Event {
    /// Monotonic sequence number within a session, starting at 1.
    pub seq: u64,
    /// Wall-clock time the event was observed.
    pub at: OffsetDateTime,
    /// What happened.
    pub kind: EventKind,
}

/// Payload of an [`Event`].
#[derive(Debug, Clone)]
pub enum EventKind {
    /// A JSON-RPC request was sent or received.
    Request {
        /// Sender side.
        direction: Direction,
        /// JSON-RPC id (number or string).
        id: Value,
        /// Method name.
        method: String,
        /// Raw params, if any.
        params: Option<Value>,
    },
    /// A JSON-RPC success response was sent or received.
    Response {
        /// Sender side of the response.
        direction: Direction,
        /// JSON-RPC id it answers.
        id: Value,
        /// Method of the request it answers, when known.
        method: Option<String>,
        /// Raw result.
        result: Value,
        /// Time since the matching request was observed.
        elapsed: Option<Duration>,
    },
    /// A JSON-RPC error response was sent or received.
    Error {
        /// Sender side of the error.
        direction: Direction,
        /// JSON-RPC id it answers, if the sender could read it.
        id: Option<Value>,
        /// Method of the request it answers, when known.
        method: Option<String>,
        /// JSON-RPC error code.
        code: i64,
        /// Human-readable message.
        message: String,
        /// Optional structured data.
        data: Option<Value>,
        /// Time since the matching request was observed.
        elapsed: Option<Duration>,
    },
    /// A JSON-RPC notification was sent or received.
    Notification {
        /// Sender side.
        direction: Direction,
        /// Method name.
        method: String,
        /// Raw params, if any.
        params: Option<Value>,
    },
    /// The session changed state.
    StateChange {
        /// New state.
        state: ConnectionState,
        /// Optional explanation (error text, quit reason).
        detail: Option<String>,
    },
    /// The server announced that a list changed; the UI should refresh it.
    ListChanged(ListKind),
    /// A subscribed resource changed.
    ResourceUpdated {
        /// Resource URI.
        uri: String,
    },
    /// `notifications/message` from the server.
    Log {
        /// Severity as sent by the server (`debug`, `info`, `warning`, ...).
        level: String,
        /// Logger name, if any.
        logger: Option<String>,
        /// Arbitrary payload.
        data: Value,
    },
    /// `notifications/progress` from the server.
    Progress {
        /// Progress token echoing the request's `_meta.progressToken`.
        token: Value,
        /// Current progress value.
        progress: f64,
        /// Total, if known.
        total: Option<f64>,
        /// Optional status text.
        message: Option<String>,
    },
    /// A line from the stdio server's stderr.
    Stderr {
        /// The line without its trailing newline.
        line: String,
    },
    /// The server asked the client for something; answer with [`ServerRequest::respond`].
    ServerRequest(ServerRequest),
    /// The server withdrew a request it sent (`notifications/cancelled`), so
    /// whatever is asking a person for its answer can stop.
    RequestCancelled {
        /// JSON-RPC id of the withdrawn request.
        id: Value,
        /// Why, if the server said.
        reason: Option<String>,
    },
    /// Something the session did on its own that the wire alone does not
    /// explain, such as reopening a subscription stream.
    Note {
        /// What it is about, usually a method name.
        topic: String,
        /// What happened.
        text: String,
        /// Whether it went wrong.
        is_error: bool,
    },
}

impl EventKind {
    /// Coarse category for filtering.
    pub fn category(&self) -> EventCategory {
        match self {
            Self::Request { .. } => EventCategory::Request,
            Self::Response { .. } => EventCategory::Response,
            Self::Error { .. } => EventCategory::Error,
            Self::Notification { .. }
            | Self::ListChanged(_)
            | Self::ResourceUpdated { .. }
            | Self::Log { .. }
            | Self::Progress { .. }
            | Self::RequestCancelled { .. } => EventCategory::Notification,
            Self::StateChange { .. } | Self::Note { .. } => EventCategory::State,
            Self::ServerRequest(_) => EventCategory::ServerRequest,
            Self::Stderr { .. } => EventCategory::Stderr,
        }
    }

    /// One-line summary suitable for a log row.
    pub fn summary(&self) -> String {
        match self {
            Self::Request {
                direction, method, ..
            } => {
                format!("{} {method}", arrow(*direction))
            }
            Self::Response {
                direction,
                method,
                elapsed,
                ..
            } => format!(
                "{} {} ok{}",
                arrow(*direction),
                method.as_deref().unwrap_or("response"),
                fmt_elapsed(*elapsed)
            ),
            Self::Error {
                direction,
                method,
                code,
                message,
                elapsed,
                ..
            } => format!(
                "{} {} error {code}: {message}{}",
                arrow(*direction),
                method.as_deref().unwrap_or("response"),
                fmt_elapsed(*elapsed)
            ),
            Self::Notification {
                direction, method, ..
            } => format!("{} {method}", arrow(*direction)),
            Self::StateChange { state, detail } => match detail {
                Some(d) => format!("{state:?}: {d}"),
                None => format!("{state:?}"),
            },
            Self::ListChanged(kind) => format!("{kind:?} list changed"),
            Self::ResourceUpdated { uri } => format!("resource updated: {uri}"),
            Self::Log {
                level,
                logger,
                data,
            } => match logger {
                Some(l) => format!("[{level}] {l}: {data}"),
                None => format!("[{level}] {data}"),
            },
            Self::Progress {
                progress,
                total,
                message,
                ..
            } => format!(
                "progress {}",
                progress_label(*progress, *total, message.as_deref())
            ),
            Self::Stderr { line } => format!("stderr: {line}"),
            Self::ServerRequest(req) => format!("server request: {}", req.kind.method()),
            Self::RequestCancelled { id, reason } => match reason {
                Some(r) => format!("server request {id} cancelled: {r}"),
                None => format!("server request {id} cancelled"),
            },
            Self::Note { topic, text, .. } => format!("{topic}: {text}"),
        }
    }
}

/// A progress notification's fields in a few words: `2 (40%): copying`.
pub fn progress_label(progress: f64, total: Option<f64>, message: Option<&str>) -> String {
    let pct = total
        .filter(|t| *t > 0.0)
        .map(|t| format!(" ({:.0}%)", progress / t * 100.0))
        .unwrap_or_default();
    let message = message.map(|m| format!(": {m}")).unwrap_or_default();
    format!("{progress}{pct}{message}")
}

fn arrow(direction: Direction) -> &'static str {
    match direction {
        Direction::Outbound => "→",
        Direction::Inbound => "←",
    }
}

fn fmt_elapsed(elapsed: Option<Duration>) -> String {
    elapsed
        .map(|e| format!(" {}ms", e.as_millis()))
        .unwrap_or_default()
}

/// A request from the server to the client that needs an answer.
#[derive(Debug, Clone)]
pub struct ServerRequest {
    /// JSON-RPC id of the server's request.
    pub id: Value,
    /// What is being asked.
    pub kind: ServerRequestKind,
    responder: Responder,
}

/// The three kinds of server-initiated requests the client handles.
#[derive(Debug, Clone, PartialEq)]
pub enum ServerRequestKind {
    /// `sampling/createMessage`: raw params as JSON.
    Sampling(Value),
    /// `elicitation/create`.
    Elicitation {
        /// Message to show the user.
        message: String,
        /// Form or URL mode.
        mode: ElicitationMode,
    },
    /// `roots/list`.
    ListRoots,
}

impl ServerRequestKind {
    /// JSON-RPC method name.
    pub fn method(&self) -> &'static str {
        match self {
            Self::Sampling(_) => "sampling/createMessage",
            Self::Elicitation { .. } => "elicitation/create",
            Self::ListRoots => "roots/list",
        }
    }
}

/// How an elicitation asks for input.
#[derive(Debug, Clone, PartialEq)]
pub enum ElicitationMode {
    /// Fill a form described by a JSON Schema.
    Form {
        /// Requested schema (restricted JSON Schema per the MCP spec).
        schema: Value,
    },
    /// Open a URL and complete the interaction there.
    Url {
        /// URL to open.
        url: String,
        /// Correlation id. A 2026-07-28 server answers the elicitation inside
        /// its round trip and sends none.
        elicitation_id: Option<String>,
    },
}

/// Answer to a [`ServerRequest`].
#[derive(Debug, Clone, PartialEq)]
pub enum ServerResponse {
    /// `CreateMessageResult` as JSON.
    Sampling(Value),
    /// Elicitation outcome.
    Elicitation {
        /// `accept`, `decline` or `cancel`.
        action: ElicitationAction,
        /// Form content when accepted.
        content: Option<Value>,
    },
    /// Roots to expose.
    Roots(Vec<Root>),
    /// Refuse with a JSON-RPC error.
    Reject {
        /// Error message returned to the server.
        message: String,
    },
}

/// User's decision on an elicitation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ElicitationAction {
    /// Submit the content.
    Accept,
    /// Explicitly refuse.
    Decline,
    /// Dismiss without answering.
    Cancel,
}

/// A filesystem root advertised to the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Root {
    /// `file://` URI.
    pub uri: String,
    /// Optional display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Cloneable single-use answer slot. The first `respond` wins.
#[derive(Debug, Clone)]
struct Responder(Arc<Mutex<Option<oneshot::Sender<ServerResponse>>>>);

impl ServerRequest {
    /// Create a request and the receiver the handler awaits.
    pub(crate) fn new(
        id: Value,
        kind: ServerRequestKind,
    ) -> (Self, oneshot::Receiver<ServerResponse>) {
        let (tx, rx) = oneshot::channel();
        let req = Self {
            id,
            kind,
            responder: Responder(Arc::new(Mutex::new(Some(tx)))),
        };
        (req, rx)
    }

    /// Deliver the answer. Returns `false` if it was already answered or the
    /// handler stopped waiting.
    pub fn respond(&self, response: ServerResponse) -> bool {
        let Ok(mut slot) = self.responder.0.lock() else {
            return false;
        };
        match slot.take() {
            Some(tx) => tx.send(response).is_ok(),
            None => false,
        }
    }

    /// Whether an answer has already been delivered.
    pub fn is_answered(&self) -> bool {
        self.responder
            .0
            .lock()
            .map(|slot| slot.is_none())
            .unwrap_or(true)
    }
}

/// Broadcast sender with sequence numbering. Cheap to clone.
#[derive(Debug, Clone)]
pub struct EventSink {
    tx: broadcast::Sender<Event>,
    seq: Arc<AtomicU64>,
}

impl EventSink {
    /// Create a sink whose subscribers keep at most `capacity` unread events.
    pub fn new(capacity: usize) -> Self {
        let (tx, _rx) = broadcast::channel(capacity.max(16));
        Self {
            tx,
            seq: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Subscribe to future events.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }

    /// Number of live subscribers.
    pub fn receiver_count(&self) -> usize {
        self.tx.receiver_count()
    }

    /// Emit an event now. Dropped silently when nobody listens.
    pub fn emit(&self, kind: EventKind) -> Event {
        let event = Event {
            seq: self.seq.fetch_add(1, Ordering::Relaxed) + 1,
            at: OffsetDateTime::now_utc(),
            kind,
        };
        let _ = self.tx.send(event.clone());
        event
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responder_answers_once() {
        let (req, rx) = ServerRequest::new(Value::from(1), ServerRequestKind::ListRoots);
        assert!(!req.is_answered());
        assert!(req.respond(ServerResponse::Roots(vec![])));
        assert!(req.is_answered());
        assert!(!req.respond(ServerResponse::Roots(vec![])));
        assert_eq!(rx.blocking_recv().unwrap(), ServerResponse::Roots(vec![]));
    }

    #[test]
    fn sink_numbers_events_and_categorizes() {
        let sink = EventSink::new(8);
        let mut rx = sink.subscribe();
        let e = sink.emit(EventKind::StateChange {
            state: ConnectionState::Connected,
            detail: None,
        });
        assert_eq!(e.seq, 1);
        assert_eq!(e.kind.category(), EventCategory::State);
        assert_eq!(rx.try_recv().unwrap().seq, 1);
        let e2 = sink.emit(EventKind::Stderr { line: "x".into() });
        assert_eq!(e2.seq, 2);
        assert_eq!(e2.kind.summary(), "stderr: x");
    }
}
