//! Transport construction from a [`ServerSpec`] and wire-level tracing.
//!
//! Hand-written because `rmcp` offers no observer hook on its transports:
//! the only way to see every raw JSON-RPC message with timing is to wrap the
//! [`Transport`] itself. [`TracedTransport`] does exactly that and nothing
//! else; all protocol logic stays in `rmcp`.

use std::collections::HashMap;
use std::fmt;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use futures::future::Either;
use rmcp::RoleClient;
use rmcp::service::{RxJsonRpcMessage, TxJsonRpcMessage};
use rmcp::transport::Transport;
use rmcp::transport::auth::AuthClient;
use rmcp::transport::streamable_http_client::{
    StreamableHttpClientTransportConfig, StreamableHttpError,
};
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};
use serde::Serialize;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::event::{Direction, EventKind, EventSink};
use crate::spec::ServerSpec;
use crate::{Error, Result};

/// Options that affect how a transport is built.
#[derive(Debug, Clone, Default)]
pub struct TransportOptions {
    /// Bearer token for HTTP servers, sent as `Authorization: Bearer <token>`.
    /// Resolved by `mcp-auth` from the keyring; never persisted here.
    pub bearer_token: Option<String>,
    /// An OAuth-aware HTTP client (attaches and refreshes tokens). Built by
    /// `mcp-auth`; takes precedence over `auth_header`.
    pub oauth: Option<AuthClient<reqwest::Client>>,
}

/// The two HTTP client flavours behind one transport type.
pub enum HttpTransport {
    /// Plain reqwest, optionally with a static `Authorization` header.
    Plain(StreamableHttpClientTransport<reqwest::Client>),
    /// rmcp's `AuthClient`: tokens from the credential store, refreshed as needed.
    OAuth(StreamableHttpClientTransport<AuthClient<reqwest::Client>>),
}

impl fmt::Debug for HttpTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Plain(_) => "HttpTransport::Plain",
            Self::OAuth(_) => "HttpTransport::OAuth",
        })
    }
}

impl Transport<RoleClient> for HttpTransport {
    type Error = StreamableHttpError<reqwest::Error>;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleClient>,
    ) -> impl Future<Output = std::result::Result<(), Self::Error>> + Send + 'static {
        match self {
            Self::Plain(t) => Either::Left(t.send(item)),
            Self::OAuth(t) => Either::Right(t.send(item)),
        }
    }

    fn receive(&mut self) -> impl Future<Output = Option<RxJsonRpcMessage<RoleClient>>> + Send {
        match self {
            Self::Plain(t) => Either::Left(t.receive()),
            Self::OAuth(t) => Either::Right(t.receive()),
        }
    }

    fn close(&mut self) -> impl Future<Output = std::result::Result<(), Self::Error>> + Send {
        match self {
            Self::Plain(t) => Either::Left(t.close()),
            Self::OAuth(t) => Either::Right(t.close()),
        }
    }
}

/// Spawn a stdio server. Returns the transport plus its captured stderr, which
/// the caller forwards to the event sink.
pub fn spawn_stdio(
    command: &str,
    args: &[String],
    env: &std::collections::BTreeMap<String, String>,
    cwd: Option<&std::path::Path>,
) -> Result<(TokioChildProcess, Option<tokio::process::ChildStderr>)> {
    let mut cmd = tokio::process::Command::new(command);
    cmd.args(args);
    for (k, v) in env {
        cmd.env(k, v);
    }
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    cmd.kill_on_drop(true);
    TokioChildProcess::builder(cmd)
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::Transport(format!("failed to spawn `{command}`: {e}")))
}

/// Bytes of one stderr line kept; what a child writes past that before a
/// newline is counted, not held, so a child that never writes one cannot
/// grow a string without bound.
pub const STDERR_LINE_LIMIT: usize = 64 * 1024;

/// Forward a child's stderr, line by line, into the event sink.
pub fn forward_stderr(
    stderr: tokio::process::ChildStderr,
    sink: EventSink,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        let mut line: Vec<u8> = Vec::new();
        let mut dropped = 0usize;
        loop {
            let chunk = match reader.fill_buf().await {
                Ok(chunk) if !chunk.is_empty() => chunk,
                _ => break,
            };
            let (take, ended) = match chunk.iter().position(|b| *b == b'\n') {
                Some(at) => (at + 1, true),
                None => (chunk.len(), false),
            };
            let body = if ended {
                &chunk[..take - 1]
            } else {
                &chunk[..take]
            };
            let room = STDERR_LINE_LIMIT.saturating_sub(line.len());
            if body.len() > room {
                line.extend_from_slice(&body[..room]);
                dropped += body.len() - room;
            } else {
                line.extend_from_slice(body);
            }
            reader.consume(take);
            if ended {
                sink.emit(EventKind::Stderr {
                    line: stderr_line(&line, dropped),
                });
                line.clear();
                dropped = 0;
            }
        }
        if !line.is_empty() {
            sink.emit(EventKind::Stderr {
                line: stderr_line(&line, dropped),
            });
        }
    })
}

/// One stderr line as text, its trailing `\r` gone and the bytes left out
/// past [`STDERR_LINE_LIMIT`] named.
fn stderr_line(bytes: &[u8], dropped: usize) -> String {
    let bytes = bytes.strip_suffix(b"\r").unwrap_or(bytes);
    let mut text = String::from_utf8_lossy(bytes).into_owned();
    if dropped > 0 {
        text.push_str(&format!(" … [{dropped} more bytes]"));
    }
    text
}

/// Build a streamable-HTTP transport for `url`.
pub fn build_http(
    url: &str,
    headers: &std::collections::BTreeMap<String, String>,
    options: &TransportOptions,
) -> Result<HttpTransport> {
    let mut config = StreamableHttpClientTransportConfig::with_uri(url.to_owned());
    if !headers.is_empty() {
        let mut map = HashMap::with_capacity(headers.len());
        for (k, v) in headers {
            let name = reqwest::header::HeaderName::from_bytes(k.as_bytes())
                .map_err(|e| Error::InvalidSpec(format!("header `{k}`: {e}")))?;
            let value = reqwest::header::HeaderValue::from_str(v)
                .map_err(|e| Error::InvalidSpec(format!("header `{k}` value: {e}")))?;
            map.insert(name, value);
        }
        config = config.custom_headers(map);
    }
    if let Some(oauth) = &options.oauth {
        return Ok(HttpTransport::OAuth(
            StreamableHttpClientTransport::with_client(oauth.clone(), config),
        ));
    }
    if let Some(token) = &options.bearer_token {
        config = config.auth_header(token.clone());
    }
    let client = reqwest::Client::builder()
        .build()
        .map_err(|e| Error::Transport(format!("http client: {e}")))?;
    Ok(HttpTransport::Plain(
        StreamableHttpClientTransport::with_client(client, config),
    ))
}

/// Validate that a spec can be turned into a transport (cheap, no I/O).
pub fn check_spec(spec: &ServerSpec) -> Result<()> {
    match spec {
        ServerSpec::Stdio { command, .. } if command.trim().is_empty() => {
            Err(Error::InvalidSpec("empty command".into()))
        }
        ServerSpec::Http { url, .. } => {
            let parsed = url::Url::parse(url)
                .map_err(|e| Error::InvalidSpec(format!("invalid url `{url}`: {e}")))?;
            if !matches!(parsed.scheme(), "http" | "https") {
                return Err(Error::InvalidSpec(format!(
                    "unsupported scheme `{}`",
                    parsed.scheme()
                )));
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

type InflightKey = (Direction, String);

/// Wraps any client transport and emits an event for every message.
pub struct TracedTransport<T> {
    inner: T,
    sink: EventSink,
    inflight: Arc<Mutex<HashMap<InflightKey, (Instant, String)>>>,
    heard: Heard,
}

/// What a traced transport knows about the server's liveness: when it last
/// received a message, and whether a request of ours still waits on an
/// answer. Cloned out before the transport is handed to the service, so the
/// session's keepalive can read it.
#[derive(Debug, Clone)]
pub struct Heard {
    at: Arc<Mutex<Instant>>,
    inflight: Arc<Mutex<HashMap<InflightKey, (Instant, String)>>>,
}

impl Heard {
    fn now(&self) {
        if let Ok(mut at) = self.at.lock() {
            *at = Instant::now();
        }
    }

    /// Time since the last message from the server arrived.
    pub fn since(&self) -> std::time::Duration {
        self.at.lock().map(|at| at.elapsed()).unwrap_or_default()
    }

    /// Strike a request this client sent, given up on before an answer:
    /// a timed-out or cancelled request must not count as work in the
    /// server's hands, or the keepalive would never ping again.
    pub fn forget(&self, id: &str) {
        if let Ok(mut map) = self.inflight.lock() {
            map.remove(&(Direction::Inbound, id.to_owned()));
        }
    }

    /// Whether a request this client sent has not been answered yet, so the
    /// server is known to have work of ours in hand.
    pub fn awaiting_server(&self) -> bool {
        self.inflight.lock().is_ok_and(|map| {
            map.keys()
                .any(|(answered_by, _)| *answered_by == Direction::Inbound)
        })
    }
}

impl<T> fmt::Debug for TracedTransport<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TracedTransport")
            .field("inner", &std::any::type_name::<T>())
            .finish_non_exhaustive()
    }
}

impl<T> TracedTransport<T> {
    /// Wrap `inner`, reporting to `sink`.
    pub fn new(inner: T, sink: EventSink) -> Self {
        let inflight = Arc::new(Mutex::new(HashMap::new()));
        Self {
            inner,
            sink,
            heard: Heard {
                at: Arc::new(Mutex::new(Instant::now())),
                inflight: inflight.clone(),
            },
            inflight,
        }
    }

    /// A handle on when the server was last heard from.
    pub fn heard(&self) -> Heard {
        self.heard.clone()
    }
}

/// Emit the event for one message. `direction` is the sender.
fn record<M: Serialize>(
    message: &M,
    direction: Direction,
    sink: &EventSink,
    inflight: &Mutex<HashMap<InflightKey, (Instant, String)>>,
) {
    let Ok(value) = serde_json::to_value(message) else {
        return;
    };
    let id = value.get("id").cloned();
    let method = value
        .get("method")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let params = value.get("params").cloned();

    // Requests initiated by `direction` are answered by the other side.
    let answered_by = match direction {
        Direction::Outbound => Direction::Inbound,
        Direction::Inbound => Direction::Outbound,
    };

    match (method, id) {
        (Some(method), Some(id)) => {
            if let Ok(mut map) = inflight.lock() {
                map.insert(
                    (answered_by, id.to_string()),
                    (Instant::now(), method.clone()),
                );
            }
            sink.emit(EventKind::Request {
                direction,
                id,
                method,
                params,
            });
        }
        (Some(method), None) => {
            sink.emit(EventKind::Notification {
                direction,
                method,
                params,
            });
        }
        (None, id) => {
            let started = id.as_ref().and_then(|id| {
                inflight
                    .lock()
                    .ok()
                    .and_then(|mut map| map.remove(&(direction, id.to_string())))
            });
            let elapsed = started.as_ref().map(|(at, _)| at.elapsed());
            let method = started.map(|(_, m)| m);
            if let Some(error) = value.get("error") {
                sink.emit(EventKind::Error {
                    direction,
                    id,
                    method,
                    code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
                    message: error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                    data: error.get("data").cloned(),
                    elapsed,
                });
            } else if let Some(id) = id {
                sink.emit(EventKind::Response {
                    direction,
                    id,
                    method,
                    result: value.get("result").cloned().unwrap_or(Value::Null),
                    elapsed,
                });
            }
        }
    }
}

impl<T: Transport<RoleClient>> Transport<RoleClient> for TracedTransport<T> {
    type Error = T::Error;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleClient>,
    ) -> impl Future<Output = std::result::Result<(), Self::Error>> + Send + 'static {
        record(&item, Direction::Outbound, &self.sink, &self.inflight);
        self.inner.send(item)
    }

    fn receive(&mut self) -> impl Future<Output = Option<RxJsonRpcMessage<RoleClient>>> + Send {
        let sink = self.sink.clone();
        let inflight = self.inflight.clone();
        let heard = self.heard.clone();
        let fut = self.inner.receive();
        async move {
            let message = fut.await;
            if let Some(message) = &message {
                heard.now();
                record(message, Direction::Inbound, &sink, &inflight);
            }
            message
        }
    }

    fn close(&mut self) -> impl Future<Output = std::result::Result<(), Self::Error>> + Send {
        self.inner.close()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::EventCategory;
    use serde_json::json;

    #[test]
    fn request_then_response_yields_elapsed_and_method() {
        let sink = EventSink::new(16);
        let mut rx = sink.subscribe();
        let inflight = Mutex::new(HashMap::new());
        record(
            &json!({"jsonrpc":"2.0","id":7,"method":"tools/list","params":{}}),
            Direction::Outbound,
            &sink,
            &inflight,
        );
        record(
            &json!({"jsonrpc":"2.0","id":7,"result":{"tools":[]}}),
            Direction::Inbound,
            &sink,
            &inflight,
        );
        let req = rx.try_recv().unwrap();
        assert_eq!(req.kind.category(), EventCategory::Request);
        let resp = rx.try_recv().unwrap();
        match resp.kind {
            EventKind::Response {
                method, elapsed, ..
            } => {
                assert_eq!(method.as_deref(), Some("tools/list"));
                assert!(elapsed.is_some());
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(inflight.lock().unwrap().is_empty());
    }

    #[test]
    fn a_request_given_up_on_no_longer_holds_the_keepalive() {
        let sink = EventSink::new(16);
        let traced = TracedTransport::new((), sink.clone());
        let heard = traced.heard();
        for id in [json!(7), json!("abc")] {
            record(
                &json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{}}),
                Direction::Outbound,
                &sink,
                &traced.inflight,
            );
            assert!(heard.awaiting_server(), "{id} is in the server's hands");
            heard.forget(&id.to_string());
            assert!(!heard.awaiting_server(), "{id} was given up on");
        }
    }

    #[test]
    fn a_stderr_line_is_cut_at_the_limit_and_says_so() {
        assert_eq!(stderr_line(b"plain\r", 0), "plain");
        assert_eq!(stderr_line(b"head", 3), "head … [3 more bytes]");
        assert_eq!(stderr_line(&[0xff, b'x'], 0), "\u{FFFD}x");
    }

    #[test]
    fn notification_and_error_are_classified() {
        let sink = EventSink::new(16);
        let mut rx = sink.subscribe();
        let inflight = Mutex::new(HashMap::new());
        record(
            &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            Direction::Outbound,
            &sink,
            &inflight,
        );
        record(
            &json!({"jsonrpc":"2.0","id":"a","error":{"code":-32601,"message":"nope"}}),
            Direction::Inbound,
            &sink,
            &inflight,
        );
        assert_eq!(
            rx.try_recv().unwrap().kind.category(),
            EventCategory::Notification
        );
        match rx.try_recv().unwrap().kind {
            EventKind::Error { code, message, .. } => {
                assert_eq!(code, -32601);
                assert_eq!(message, "nope");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn check_spec_rejects_bad_urls() {
        assert!(
            check_spec(&ServerSpec::Http {
                url: "ftp://x".into(),
                headers: Default::default(),
                auth: crate::AuthRef::None,
            })
            .is_err()
        );
        assert!(
            check_spec(&ServerSpec::Http {
                url: "https://example.com/mcp".into(),
                headers: Default::default(),
                auth: crate::AuthRef::None,
            })
            .is_ok()
        );
    }
}
