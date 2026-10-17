//! The HTTP side: who may connect, which server a path means, and the
//! streamable HTTP an agent speaks.

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::frame::client_info;
use crate::observe::Observer;
use crate::relay::{Posted, Relay};
use crate::upstream::{Connector, Frame, RequestContext, StderrSink, UpstreamError};

/// Largest message an agent may post, in bytes.
const MAX_BODY: usize = 32 * 1024 * 1024;
/// Agent connections one server takes at once, by default.
const MAX_CONNECTIONS: usize = 16;
/// How long a connection nobody uses is kept, by default.
const IDLE_LIMIT: Duration = Duration::from_secs(30 * 60);
/// How often connections are checked for that, by default.
const SWEEP: Duration = Duration::from_secs(30);
/// How long stopping may wait for the streams to end.
const STOP_AFTER: Duration = Duration::from_secs(3);
const SESSION: &str = "mcp-session-id";

/// A server the gateway answers for.
#[derive(Clone)]
pub struct RelayServer {
    /// The path it answers on: `/<namespace>`. See [`valid_namespace`].
    pub namespace: String,
    /// How to open it for an agent.
    pub connector: Arc<dyn Connector>,
}

impl std::fmt::Debug for RelayServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RelayServer")
            .field("namespace", &self.namespace)
            .finish_non_exhaustive()
    }
}

/// Whether `name` can be a namespace: letters, digits, `_` and `-`, up to 64.
pub fn valid_namespace(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// How much the gateway lets an agent have.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Agent connections one server takes at once.
    pub connections: usize,
    /// How long a connection nobody uses is kept.
    pub idle: Duration,
    /// How often connections are checked for that.
    pub sweep: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            connections: MAX_CONNECTIONS,
            idle: IDLE_LIMIT,
            sweep: SWEEP,
        }
    }
}

struct Shared {
    token: String,
    port: u16,
    limits: Limits,
    /// Set once stopping begins: nothing new is accepted.
    stopping: AtomicBool,
    /// Connections being opened, by server, so the cap holds for agents that
    /// arrive together.
    opening: Mutex<HashMap<String, usize>>,
    servers: RwLock<HashMap<String, Arc<dyn Connector>>>,
    relays: Mutex<HashMap<String, Arc<Relay>>>,
    observer: Arc<dyn Observer>,
}

/// A running gateway.
#[derive(Debug)]
pub struct Gateway {
    addr: SocketAddr,
    shared: Arc<Shared>,
    cancel: CancellationToken,
    serving: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shared")
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

impl Gateway {
    /// Listen on `127.0.0.1:port` (0 for any free port) for agents that
    /// present `token`, answering for `servers`.
    pub async fn start(
        port: u16,
        token: String,
        servers: Vec<RelayServer>,
        observer: Arc<dyn Observer>,
    ) -> std::io::Result<Self> {
        Self::start_with(port, token, servers, observer, Limits::default()).await
    }

    /// [`Self::start`], with other limits.
    pub async fn start_with(
        port: u16,
        token: String,
        servers: Vec<RelayServer>,
        observer: Arc<dyn Observer>,
        limits: Limits,
    ) -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", port)).await?;
        let addr = listener.local_addr()?;
        let shared = Arc::new(Shared {
            token,
            port: addr.port(),
            limits,
            stopping: AtomicBool::new(false),
            opening: Mutex::new(HashMap::new()),
            servers: RwLock::new(
                servers
                    .into_iter()
                    .filter(|s| valid_namespace(&s.namespace))
                    .map(|s| (s.namespace, s.connector))
                    .collect(),
            ),
            relays: Mutex::new(HashMap::new()),
            observer,
        });
        let app = Router::new().fallback(handle).with_state(shared.clone());
        let cancel = CancellationToken::new();
        let stopping = cancel.clone();
        let sweeping = shared.clone();
        let serving = tokio::spawn(async move {
            let serve = axum::serve(listener, app)
                .with_graceful_shutdown(stopping.clone().cancelled_owned());
            tokio::select! {
                result = serve.into_future() => {
                    if let Err(e) = result {
                        tracing::warn!("the gateway stopped serving: {e}");
                    }
                }
                () = sweep(sweeping, stopping) => {}
            }
        });
        Ok(Self {
            addr,
            shared,
            cancel,
            serving: Mutex::new(Some(serving)),
        })
    }

    /// The address it listens on.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Change the servers it answers for. Agent connections to a server that
    /// is gone, or opened differently now, end.
    pub async fn set_servers(&self, servers: Vec<RelayServer>) {
        let next: HashMap<String, Arc<dyn Connector>> = servers
            .into_iter()
            .filter(|s| valid_namespace(&s.namespace))
            .map(|s| (s.namespace, s.connector))
            .collect();
        let stale: Vec<Arc<Relay>> = {
            let Ok(mut current) = self.shared.servers.write() else {
                return;
            };
            let same = |namespace: &str| match (current.get(namespace), next.get(namespace)) {
                (Some(a), Some(b)) => std::ptr::addr_eq(Arc::as_ptr(a), Arc::as_ptr(b)),
                _ => false,
            };
            let stale = self
                .shared
                .relays
                .lock()
                .map(|relays| {
                    relays
                        .values()
                        .filter(|r| !same(&r.namespace))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            *current = next.clone();
            stale
        };
        for relay in stale {
            relay.close("the server was changed or removed").await;
        }
    }

    /// How many agents are connected now.
    pub fn connections(&self) -> usize {
        self.shared
            .relays
            .lock()
            .map_or(0, |r| r.values().filter(|r| !r.is_closed()).count())
    }

    /// Stop: end every agent connection and the servers behind them. Stopping
    /// a gateway that has stopped does nothing.
    pub async fn stop(&self) {
        // Nothing new from here on, then every connection there is, ended
        // together so that one server slow to close does not hold the rest.
        self.shared.stopping.store(true, Ordering::SeqCst);
        for _ in 0..2 {
            let relays: Vec<Arc<Relay>> = self
                .shared
                .relays
                .lock()
                .map(|r| r.values().filter(|r| !r.is_closed()).cloned().collect())
                .unwrap_or_default();
            futures::future::join_all(relays.iter().map(|r| r.close("the gateway stopped"))).await;
        }
        self.cancel.cancel();
        let serving = self.serving.lock().ok().and_then(|mut s| s.take());
        if let Some(serving) = serving {
            let _ = tokio::time::timeout(STOP_AFTER, serving).await;
        }
    }
}

/// Close what nobody uses, and forget what ended.
async fn sweep(shared: Arc<Shared>, stop: CancellationToken) {
    loop {
        tokio::select! {
            () = stop.cancelled() => return,
            () = tokio::time::sleep(shared.limits.sweep) => {}
        }
        let (idle, ended): (Vec<_>, Vec<_>) = shared
            .relays
            .lock()
            .map(|r| r.values().cloned().partition(|r| !r.is_closed()))
            .unwrap_or_default();
        for relay in idle.iter().filter(|r| r.is_idle(shared.limits.idle)) {
            relay.close("the agent was idle for too long").await;
        }
        if let Ok(mut relays) = shared.relays.lock() {
            for relay in ended {
                relays.remove(&relay.id);
            }
        }
    }
}

impl Shared {
    /// What an agent must have right, or the answer that turns it away.
    fn check(&self, headers: &HeaderMap) -> Option<Response> {
        let port = self.port;
        let hosts = [
            format!("127.0.0.1:{port}"),
            format!("localhost:{port}"),
            format!("[::1]:{port}"),
        ];
        let host_ok = headers
            .get(header::HOST)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|host| hosts.iter().any(|h| h.eq_ignore_ascii_case(host)));
        // A page in a browser always names itself in `Origin`; an agent that
        // is a program usually does not. Only this machine may.
        let origin_ok = headers.get(header::ORIGIN).is_none_or(|origin| {
            origin.to_str().is_ok_and(|origin| {
                hosts
                    .iter()
                    .any(|h| format!("http://{h}").eq_ignore_ascii_case(origin))
            })
        });
        if !host_ok || !origin_ok {
            return Some(rpc_error(
                StatusCode::FORBIDDEN,
                -32600,
                "Forbidden: this gateway answers only to this machine",
            ));
        }
        let presented = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "));
        if !presented.is_some_and(|t| same(t.as_bytes(), self.token.as_bytes())) {
            let mut response = rpc_error(
                StatusCode::UNAUTHORIZED,
                -32600,
                "Unauthorized: send the gateway's access token as a bearer token",
            );
            response.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                HeaderValue::from_static("Bearer realm=\"coco-mcp-gateway\""),
            );
            return Some(response);
        }
        None
    }

    fn connector(&self, namespace: &str) -> Option<Arc<dyn Connector>> {
        self.servers.read().ok()?.get(namespace).cloned()
    }

    fn relay(&self, id: &str, namespace: &str) -> Option<Arc<Relay>> {
        let relay = self.relays.lock().ok()?.get(id).cloned()?;
        (relay.namespace == namespace && !relay.is_closed()).then_some(relay)
    }
}

/// Whether two byte strings are equal, taking as long either way.
fn same(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

async fn handle(State(shared): State<Arc<Shared>>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    if let Some(refused) = shared.check(&parts.headers) {
        return refused;
    }
    if shared.stopping.load(Ordering::SeqCst) {
        return rpc_error(
            StatusCode::SERVICE_UNAVAILABLE,
            -32603,
            "The gateway is stopping",
        );
    }
    let namespace = parts.uri.path().trim_matches('/').to_owned();
    let Some(connector) = shared.connector(&namespace) else {
        return rpc_error(
            StatusCode::NOT_FOUND,
            -32600,
            "No server answers on this path",
        );
    };
    match parts.method {
        Method::POST => post(&shared, &namespace, connector, &parts.headers, body).await,
        Method::GET => listen(&shared, &namespace, &parts.headers),
        Method::DELETE => end(&shared, &namespace, &parts.headers).await,
        _ => {
            let mut response =
                rpc_error(StatusCode::METHOD_NOT_ALLOWED, -32600, "Method not allowed");
            response
                .headers_mut()
                .insert(header::ALLOW, HeaderValue::from_static("GET, POST, DELETE"));
            response
        }
    }
}

fn session_of(headers: &HeaderMap) -> Option<&str> {
    headers.get(SESSION).and_then(|v| v.to_str().ok())
}

async fn post(
    shared: &Arc<Shared>,
    namespace: &str,
    connector: Arc<dyn Connector>,
    headers: &HeaderMap,
    body: Body,
) -> Response {
    let Ok(raw) = to_bytes(body, MAX_BODY).await else {
        return rpc_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            -32600,
            "The message is too large",
        );
    };
    let value: Value = match serde_json::from_slice(&raw) {
        Ok(value) => value,
        Err(_) => return rpc_error(StatusCode::BAD_REQUEST, -32700, "Parse error"),
    };
    if value.is_array() {
        return rpc_error(
            StatusCode::BAD_REQUEST,
            -32600,
            "Batches are not supported: post one message at a time",
        );
    }
    let (relay, created) = match session_of(headers) {
        Some(id) => match shared.relay(id, namespace) {
            Some(relay) => (relay, false),
            None => {
                return rpc_error(StatusCode::NOT_FOUND, -32600, "Unknown or ended session");
            }
        },
        None => {
            // A connection starts a process or a session of its own, so a
            // message that is not the start of one does not make it.
            let method = value.get("method").and_then(Value::as_str);
            if !matches!(method, Some("initialize" | "server/discover")) {
                return rpc_error(
                    StatusCode::BAD_REQUEST,
                    -32600,
                    "Start with initialize, and send the Mcp-Session-Id it answers with",
                );
            }
            match connect(shared, namespace, &connector).await {
                Ok(relay) => (relay, true),
                Err(response) => return *response,
            }
        }
    };
    if created && let Some(info) = client_info(&value) {
        shared.observer.client(namespace, &relay.id, info);
    }
    let context = RequestContext {
        headers: headers
            .iter()
            .filter(|(name, _)| name.as_str().starts_with("mcp-") && name.as_str() != SESSION)
            .filter_map(|(name, value)| Some((name.to_string(), value.to_str().ok()?.to_owned())))
            .collect(),
    };
    let posted = relay.post(Frame { value, raw }, &context).await;
    let mut response = match posted {
        Ok(Posted::Accepted) => StatusCode::ACCEPTED.into_response(),
        Ok(Posted::Stream(messages)) => stream_response(messages),
        Err(e) => {
            if created {
                relay.close("the first message could not be sent").await;
            }
            upstream_error(e)
        }
    };
    if let Ok(value) = HeaderValue::from_str(&relay.id) {
        response
            .headers_mut()
            .insert(HeaderName::from_static(SESSION), value);
    }
    response
}

/// Open the server for a new agent connection.
async fn connect(
    shared: &Arc<Shared>,
    namespace: &str,
    connector: &Arc<dyn Connector>,
) -> Result<Arc<Relay>, Box<Response>> {
    // Count what is open and what is being opened, and reserve a place, in
    // one step: agents that arrive together cannot all see room.
    let reserved = {
        let (Ok(relays), Ok(mut opening)) = (shared.relays.lock(), shared.opening.lock()) else {
            return Err(Box::new(rpc_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                -32603,
                "The gateway is in an unusable state",
            )));
        };
        let open = relays
            .values()
            .filter(|r| r.namespace == namespace && !r.is_closed())
            .count();
        let pending = opening.entry(namespace.to_owned()).or_insert(0);
        if open + *pending >= shared.limits.connections {
            false
        } else {
            *pending += 1;
            true
        }
    };
    if !reserved {
        return Err(Box::new(rpc_error(
            StatusCode::TOO_MANY_REQUESTS,
            -32600,
            "Too many agents are connected to this server",
        )));
    }
    let release = || {
        if let Ok(mut opening) = shared.opening.lock()
            && let Some(pending) = opening.get_mut(namespace)
        {
            *pending = pending.saturating_sub(1);
        }
    };
    let id = uuid::Uuid::new_v4().simple().to_string();
    let stderr: StderrSink = {
        let observer = shared.observer.clone();
        let (namespace, id) = (namespace.to_owned(), id.clone());
        Arc::new(move |line| observer.stderr(&namespace, &id, &line))
    };
    let opened = match connector.open(stderr).await {
        Ok(opened) => opened,
        Err(e) => {
            release();
            return Err(Box::new(upstream_error(e)));
        }
    };
    let relay = Relay::start(
        namespace.to_owned(),
        id.clone(),
        opened,
        shared.observer.clone(),
    );
    // The server may have been changed, or the gateway stopped, while it was
    // opening: a connection to what is no longer answered for is not kept.
    let current = shared
        .connector(namespace)
        .is_some_and(|now| std::ptr::addr_eq(Arc::as_ptr(&now), Arc::as_ptr(connector)));
    if !current || shared.stopping.load(Ordering::SeqCst) {
        release();
        relay.close("the server changed while it was opening").await;
        return Err(Box::new(rpc_error(
            StatusCode::SERVICE_UNAVAILABLE,
            -32603,
            "The server changed while it was opening: try again",
        )));
    }
    if let Ok(mut relays) = shared.relays.lock() {
        relays.insert(id.clone(), relay.clone());
    }
    release();
    shared.observer.opened(namespace, &id);
    Ok(relay)
}

fn listen(shared: &Arc<Shared>, namespace: &str, headers: &HeaderMap) -> Response {
    let Some(relay) = session_of(headers).and_then(|id| shared.relay(id, namespace)) else {
        return rpc_error(StatusCode::NOT_FOUND, -32600, "Unknown or ended session");
    };
    match relay.listen() {
        Some(messages) => stream_response(messages),
        None => rpc_error(
            StatusCode::CONFLICT,
            -32600,
            "This session already has a stream open",
        ),
    }
}

async fn end(shared: &Arc<Shared>, namespace: &str, headers: &HeaderMap) -> Response {
    let Some(relay) = session_of(headers).and_then(|id| shared.relay(id, namespace)) else {
        return rpc_error(StatusCode::NOT_FOUND, -32600, "Unknown or ended session");
    };
    relay.close("the agent ended the session").await;
    StatusCode::NO_CONTENT.into_response()
}

/// The messages of a request, or of the agent's stream, as server-sent
/// events, ending when the sender does.
fn stream_response(messages: mpsc::Receiver<Value>) -> Response {
    let events = futures::stream::unfold(messages, |mut messages| async move {
        let message = messages.recv().await?;
        Some((
            Ok::<Event, Infallible>(Event::default().data(message.to_string())),
            messages,
        ))
    });
    Sse::new(events)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response()
}

/// What an error from the server is to the agent: the same HTTP answer when
/// it was one, else a gateway error.
fn upstream_error(error: UpstreamError) -> Response {
    match error {
        // The server turned the gateway away, not the agent: the agent is not
        // sent to authorize against the gateway over it.
        UpstreamError::Http {
            status: status @ (401 | 403),
            body,
            ..
        } => {
            let detail = String::from_utf8_lossy(&body);
            let detail: String = detail.chars().take(300).collect();
            rpc_error(
                StatusCode::BAD_GATEWAY,
                -32603,
                &format!("The server refused the gateway's credentials (HTTP {status}): {detail}"),
            )
        }
        UpstreamError::Http {
            status,
            headers,
            body,
        } => {
            let mut response = Response::new(Body::from(Bytes::from(body)));
            *response.status_mut() =
                StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
            for (name, value) in headers {
                if let (Ok(name), Ok(value)) = (
                    HeaderName::from_bytes(name.as_bytes()),
                    HeaderValue::from_str(&value),
                ) {
                    response.headers_mut().insert(name, value);
                }
            }
            response
        }
        UpstreamError::Closed => rpc_error(
            StatusCode::BAD_GATEWAY,
            -32603,
            "The server's session ended",
        ),
        UpstreamError::Failed(message) => rpc_error(StatusCode::BAD_GATEWAY, -32603, &message),
    }
}

fn rpc_error(status: StatusCode, code: i64, message: &str) -> Response {
    let body = json!({
        "jsonrpc": "2.0",
        "id": null,
        "error": { "code": code, "message": message },
    });
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response()
}
