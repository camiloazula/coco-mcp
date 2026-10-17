//! The root: every included server merged into one, for an agent that
//! connects once. Where a path relays one server unchanged, this one answers
//! the agent as a server of its own and speaks to each real server as a
//! client, so it does interpret the protocol, and only as far as merging
//! needs.
//!
//! - Tools and prompts are listed as `namespace__name` and routed by that
//!   prefix. Resources keep the URIs their server sent and are routed to the
//!   server that listed them, linked them in a tool result, or has a
//!   template for them.
//! - A request goes to one server under the agent's own id, so what comes
//!   back is the server's answer to it. What the server asks of the agent
//!   (sampling, roots, elicitation) carries an id that says which server
//!   asked, and the agent's answer goes back to that one.
//! - Lists are asked of every server and put together; a server that fails
//!   is left out of the answer, not the answer left out.
//!
//! Only the `initialize` handshake is spoken: an agent that asks
//! `server/discover` is told the method is not there and falls back to it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::future::join_all;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::Direction;
use crate::frame::{Kind, client_info, id_key, kind};
use crate::merge::{self, Owners};
use crate::observe::{ClientInfo, Observer, Traffic};
use crate::relay::{Posted, Session};
use crate::streams::Streams;
use crate::upstream::{
    Connector, Frame, Opened, RequestContext, StderrSink, Upstream, UpstreamError,
};

/// How long a server may take to start and answer `initialize`.
const OPEN_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a server may take to answer a list.
const LIST_TIMEOUT: Duration = Duration::from_secs(30);
/// Pages of one list read before giving up on it.
const MAX_PAGES: usize = 100;
/// What the gateway calls itself to the agent.
const NAME: &str = "coco-mcp-gateway";
/// The id of the handshake with each server.
const INIT: &str = "coco:init";
/// Requests one agent may have being answered at once.
const MAX_INFLIGHT: usize = 256;

/// Whether `id` is of the kind the gateway makes for itself, which an agent
/// may not use: it would meet the answer to one of the gateway's own.
fn reserved(id: &Value) -> bool {
    id.as_str()
        .is_some_and(|id| id.starts_with("coco:") || id.starts_with("coco-"))
}

/// What waits for a server's response.
enum Waiter {
    /// A question of the gateway's own: a list, a level.
    Internal(oneshot::Sender<Value>),
    /// The agent's request, forwarded under its own id.
    Agent,
}

struct Waiting {
    waiter: Waiter,
    method: String,
    at: Instant,
}

/// One real server behind the root.
struct Member {
    namespace: String,
    upstream: Arc<dyn Upstream>,
    capabilities: Value,
    /// The protocol version it settled on, sent with what goes to it after.
    version: String,
    waiting: Mutex<HashMap<String, Waiting>>,
    down: AtomicBool,
}

/// The root, for one agent.
pub(crate) struct Aggregate {
    me: Weak<Aggregate>,
    id: String,
    members: Vec<Member>,
    streams: Streams,
    observer: Arc<dyn Observer>,
    owners: Mutex<Owners>,
    /// Which server each forwarded request went to, by the agent's id.
    inflight: Mutex<HashMap<String, usize>>,
    /// Whether the resource lists were read, so a read of a URI the agent
    /// never listed can look for its server. Held while they are read, so
    /// reads that arrive together wait for it.
    listed: tokio::sync::Mutex<bool>,
    /// A list failed while the lists were read: they are read again.
    list_failed: AtomicBool,
    /// Places for requests being answered.
    permits: Arc<tokio::sync::Semaphore>,
    next: AtomicU64,
    ended: CancellationToken,
}

fn ok(id: &Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn failure(id: &Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn request(id: &Value, method: &str, params: &Value) -> Value {
    let mut message = json!({"jsonrpc": "2.0", "id": id, "method": method});
    if !params.is_null()
        && let Some(object) = message.as_object_mut()
    {
        object.insert("params".into(), params.clone());
    }
    message
}

fn frame_of(value: Value) -> Frame {
    let raw = bytes::Bytes::from(value.to_string());
    Frame { value, raw }
}

impl Aggregate {
    /// Open every server for an agent that sent `initialize`, and answer it.
    /// A server that cannot be opened is left out and says so in its log;
    /// with none opened, there is no root to offer.
    pub(crate) async fn open(
        id: String,
        servers: &[(String, Arc<dyn Connector>)],
        initialize: &Value,
        observer: Arc<dyn Observer>,
    ) -> Result<(Arc<Self>, Value), UpstreamError> {
        let params = initialize.get("params").cloned().unwrap_or(Value::Null);
        let info = client_info(initialize);
        let opening = servers.iter().map(|(namespace, connector)| {
            open_member(
                namespace.clone(),
                connector.clone(),
                &params,
                &id,
                info.clone(),
                observer.clone(),
            )
        });
        let mut members = Vec::new();
        let mut incoming = Vec::new();
        // A server that could not be opened said so in its own log, in
        // `open_member`, and is left out.
        for (member, rx) in join_all(opening).await.into_iter().flatten() {
            members.push(member);
            incoming.push(rx);
        }
        if members.is_empty() {
            return Err(UpstreamError::Failed(
                "none of the servers could be opened".into(),
            ));
        }
        let capabilities = merge::capabilities(members.iter().map(|m| &m.capabilities));
        let requested = params
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or("2025-11-25");
        let version = merge::negotiated(
            requested,
            &members
                .iter()
                .map(|m| m.version.as_str())
                .collect::<Vec<_>>(),
        );
        let aggregate = Arc::new_cyclic(|me| Self {
            me: me.clone(),
            id,
            members,
            streams: Streams::new(),
            observer,
            owners: Mutex::new(Owners::default()),
            inflight: Mutex::new(HashMap::new()),
            listed: tokio::sync::Mutex::new(false),
            list_failed: AtomicBool::new(false),
            permits: Arc::new(tokio::sync::Semaphore::new(MAX_INFLIGHT)),
            next: AtomicU64::new(1),
            ended: CancellationToken::new(),
        });
        for (idx, rx) in incoming.into_iter().enumerate() {
            tokio::spawn(aggregate.clone().pump(idx, rx));
        }
        let result = json!({
            "protocolVersion": version,
            "capabilities": capabilities,
            "serverInfo": {"name": NAME, "version": env!("CARGO_PKG_VERSION")},
        });
        Ok((aggregate, result))
    }

    fn up(&self) -> impl Iterator<Item = (usize, &Member)> {
        self.members
            .iter()
            .enumerate()
            .filter(|(_, m)| !m.down.load(Ordering::SeqCst))
    }

    fn namespaces(&self) -> Vec<&str> {
        self.up().map(|(_, m)| m.namespace.as_str()).collect()
    }

    /// Send `message` to member `idx`, reporting it.
    async fn send(&self, idx: usize, message: Value) -> Result<(), UpstreamError> {
        let member = &self.members[idx];
        self.observer.frame(
            &member.namespace,
            &self.id,
            Traffic {
                direction: Direction::Outbound,
                frame: &message,
                method: None,
                elapsed: None,
            },
        );
        let context = RequestContext {
            headers: vec![("mcp-protocol-version".into(), member.version.clone())],
        };
        member.upstream.send(&frame_of(message), &context).await
    }

    /// Send a request to member `idx` and note what waits for its response.
    async fn ask_with(
        &self,
        idx: usize,
        id: &Value,
        method: &str,
        params: &Value,
        waiter: Waiter,
    ) -> Result<(), UpstreamError> {
        let key = id_key(id);
        if let Ok(mut waiting) = self.members[idx].waiting.lock() {
            waiting.insert(
                key.clone(),
                Waiting {
                    waiter,
                    method: method.to_owned(),
                    at: Instant::now(),
                },
            );
        }
        let sent = self.send(idx, request(id, method, params)).await;
        if sent.is_err()
            && let Ok(mut waiting) = self.members[idx].waiting.lock()
        {
            waiting.remove(&key);
        }
        sent
    }

    /// A question of the gateway's own, answered with the result.
    async fn ask(&self, idx: usize, method: &str, params: Value) -> Result<Value, String> {
        let id = json!(format!("coco:{}", self.next.fetch_add(1, Ordering::SeqCst)));
        let (tx, rx) = oneshot::channel();
        self.ask_with(idx, &id, method, &params, Waiter::Internal(tx))
            .await
            .map_err(|e| e.to_string())?;
        let waited = tokio::select! {
            () = self.ended.cancelled() => Err("the agent left".to_owned()),
            response = tokio::time::timeout(LIST_TIMEOUT, rx) => response
                .map_err(|_| "it did not answer in time".to_owned()),
        };
        let response = match waited {
            Ok(response) => response.map_err(|_| "it went away".to_owned())?,
            Err(why) => {
                if let Ok(mut waiting) = self.members[idx].waiting.lock() {
                    waiting.remove(&id_key(&id));
                }
                return Err(why);
            }
        };
        match response.get("error") {
            Some(error) => Err(error["message"].as_str().unwrap_or("it refused").to_owned()),
            None => Ok(response.get("result").cloned().unwrap_or(Value::Null)),
        }
    }

    /// Every page of a list of member `idx`.
    async fn list_all(&self, idx: usize, method: &str, field: &str) -> Result<Vec<Value>, String> {
        let mut items = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let params = match &cursor {
                Some(cursor) => json!({"cursor": cursor}),
                None => json!({}),
            };
            let page = self.ask(idx, method, params).await?;
            if let Some(found) = page.get(field).and_then(Value::as_array) {
                items.extend(found.iter().cloned());
            }
            cursor = page
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if cursor.is_none() {
                break;
            }
        }
        Ok(items)
    }

    /// A list asked of every server that can have one, put together.
    async fn merged(&self, method: &str, field: &str, capability: &str) -> Vec<Value> {
        let asking = self
            .up()
            .filter(|(_, m)| m.capabilities.get(capability).is_some())
            .map(|(idx, _)| async move { (idx, self.list_all(idx, method, field).await) });
        let mut merged = Vec::new();
        let mut listed: Vec<(String, Vec<String>)> = Vec::new();
        // What was already listed by a server before: a URI or template two
        // servers hold is the first one's to serve and is listed once.
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for (idx, result) in join_all(asking).await {
            let namespace = self.members[idx].namespace.clone();
            let items = match result {
                Ok(items) => items,
                Err(why) => {
                    self.list_failed.store(true, Ordering::SeqCst);
                    self.observer.stderr(
                        &namespace,
                        &self.id,
                        &format!("{method} failed, so this server is left out of the list: {why}"),
                    );
                    continue;
                }
            };
            if let Ok(mut owners) = self.owners.lock() {
                match field {
                    "resources" => owners.forget_uris(&namespace),
                    "resourceTemplates" => owners.forget_templates(&namespace),
                    _ => {}
                }
            }
            let mut uris = Vec::new();
            for mut item in items {
                match field {
                    "tools" | "prompts" => {
                        let Some(name) = item.get("name").and_then(Value::as_str) else {
                            continue;
                        };
                        let Some(name) = merge::prefixed(&namespace, name) else {
                            self.observer.stderr(
                                &namespace,
                                &self.id,
                                &format!("a name is too long to list at the root: {name}"),
                            );
                            continue;
                        };
                        item["name"] = Value::String(name);
                    }
                    "resources" => {
                        if let Some(uri) = item.get("uri").and_then(Value::as_str) {
                            uris.push(uri.to_owned());
                            if let Ok(mut owners) = self.owners.lock() {
                                owners.uri(&namespace, uri);
                            }
                            if !seen.insert(uri.to_owned()) {
                                continue;
                            }
                        }
                    }
                    _ => {
                        if let Some(template) = item.get("uriTemplate").and_then(Value::as_str) {
                            if let Ok(mut owners) = self.owners.lock() {
                                owners.template(&namespace, template);
                            }
                            if !seen.insert(template.to_owned()) {
                                continue;
                            }
                        }
                    }
                }
                merged.push(item);
            }
            listed.push((namespace, uris));
        }
        for (uri, namespace) in Owners::clashes(&listed) {
            self.observer.stderr(
                &namespace,
                &self.id,
                &format!(
                    "{uri} is also listed by a server before this one, which serves it at the root"
                ),
            );
        }
        merged
    }

    /// The resource lists, read once, so an owner can be found for a URI.
    async fn ensure_listed(&self) {
        let mut done = self.listed.lock().await;
        if *done {
            return;
        }
        self.list_failed.store(false, Ordering::SeqCst);
        self.merged("resources/list", "resources", "resources")
            .await;
        self.merged("resources/templates/list", "resourceTemplates", "resources")
            .await;
        // A list that failed is asked again by the next read that needs it.
        *done = !self.list_failed.load(Ordering::SeqCst);
    }

    /// Answer a request the agent made.
    async fn handle(self: Arc<Self>, id: Value, method: String, params: Value) {
        let key = id_key(&id);
        let reply = match method.as_str() {
            _ if reserved(&id) => Some(failure(
                &id,
                -32600,
                "Ids that start with coco: or coco- are the gateway's own",
            )),
            "ping" => Some(ok(&id, json!({}))),
            "initialize" => Some(failure(&id, -32600, "already initialized")),
            "tools/list" => Some(ok(
                &id,
                json!({"tools": self.merged("tools/list", "tools", "tools").await}),
            )),
            "prompts/list" => Some(ok(
                &id,
                json!({"prompts": self.merged("prompts/list", "prompts", "prompts").await}),
            )),
            "resources/list" => Some(ok(
                &id,
                json!({"resources": self.merged("resources/list", "resources", "resources").await}),
            )),
            "resources/templates/list" => Some(ok(
                &id,
                json!({"resourceTemplates": self.merged(
                    "resources/templates/list", "resourceTemplates", "resources").await}),
            )),
            "tools/call" | "prompts/get" => self.forward_named(&id, &method, params).await,
            "resources/read" | "resources/subscribe" | "resources/unsubscribe" => {
                self.forward_uri(&id, &method, params).await
            }
            "completion/complete" => self.forward_completion(&id, params).await,
            "logging/setLevel" => {
                let asking = self
                    .up()
                    .filter(|(_, m)| m.capabilities.get("logging").is_some())
                    .map(|(idx, _)| self.ask(idx, "logging/setLevel", params.clone()));
                join_all(asking).await;
                Some(ok(&id, json!({})))
            }
            _ => Some(failure(&id, -32601, "Method not found")),
        };
        if let Some(reply) = reply {
            self.streams.answer(&key, reply).await;
        }
    }

    /// Forward a request to member `idx` under the agent's own id. `None`
    /// when it was sent, for the answer comes back through the pump.
    async fn forward(&self, idx: usize, id: &Value, method: &str, params: Value) -> Option<Value> {
        if let Ok(mut inflight) = self.inflight.lock() {
            inflight.insert(id_key(id), idx);
        }
        match self.ask_with(idx, id, method, &params, Waiter::Agent).await {
            Ok(()) => None,
            Err(e) => {
                if let Ok(mut inflight) = self.inflight.lock() {
                    inflight.remove(&id_key(id));
                }
                Some(failure(id, -32603, &e.to_string()))
            }
        }
    }

    /// `tools/call` and `prompts/get`: the name says which server.
    async fn forward_named(&self, id: &Value, method: &str, mut params: Value) -> Option<Value> {
        let Some(merged) = params
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            return Some(failure(id, -32602, "No name was given"));
        };
        let Some((namespace, name)) = merge::split(&merged, self.namespaces()) else {
            return Some(failure(
                id,
                -32602,
                &format!(
                    "Unknown name `{merged}`: names here are `<server>{}<name>`",
                    merge::SEPARATOR
                ),
            ));
        };
        let Some(idx) = self.index_of(namespace) else {
            return Some(failure(id, -32603, "The server is not here any more"));
        };
        params["name"] = Value::String(name.to_owned());
        self.forward(idx, id, method, params).await
    }

    fn index_of(&self, namespace: &str) -> Option<usize> {
        self.members.iter().position(|m| m.namespace == namespace)
    }

    /// The server that serves `uri`, looking for it once if nothing was listed.
    async fn owner_of(&self, uri: &str) -> Option<usize> {
        for round in 0..2 {
            let found = {
                let owners = self.owners.lock().ok()?;
                owners.owner(uri, &self.namespaces()).map(str::to_owned)
            };
            if let Some(namespace) = found {
                return self.index_of(&namespace);
            }
            if round == 0 {
                self.ensure_listed().await;
            }
        }
        None
    }

    /// A request about a resource: the URI says which server.
    async fn forward_uri(&self, id: &Value, method: &str, params: Value) -> Option<Value> {
        let Some(uri) = params.get("uri").and_then(Value::as_str).map(str::to_owned) else {
            return Some(failure(id, -32602, "No uri was given"));
        };
        match self.owner_of(&uri).await {
            Some(idx) => self.forward(idx, id, method, params).await,
            None => Some(json!({"jsonrpc": "2.0", "id": id, "error": {
                "code": -32002, "message": "Resource not found", "data": {"uri": uri}}})),
        }
    }

    /// `completion/complete`: a prompt by its name, a resource by its URI.
    async fn forward_completion(&self, id: &Value, mut params: Value) -> Option<Value> {
        let reference = params.get("ref").cloned().unwrap_or(Value::Null);
        match reference.get("type").and_then(Value::as_str) {
            Some("ref/prompt") => {
                let Some(merged) = reference.get("name").and_then(Value::as_str) else {
                    return Some(failure(id, -32602, "No prompt name was given"));
                };
                let Some((namespace, name)) = merge::split(merged, self.namespaces()) else {
                    return Some(failure(id, -32602, "Unknown prompt"));
                };
                let Some(idx) = self.index_of(namespace) else {
                    return Some(failure(id, -32603, "The server is not here any more"));
                };
                params["ref"]["name"] = Value::String(name.to_owned());
                self.forward(idx, id, "completion/complete", params).await
            }
            Some("ref/resource") => {
                let Some(uri) = reference
                    .get("uri")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                else {
                    return Some(failure(id, -32602, "No uri was given"));
                };
                match self.owner_of(&uri).await {
                    Some(idx) => self.forward(idx, id, "completion/complete", params).await,
                    None => Some(failure(id, -32002, "Resource not found")),
                }
            }
            _ => Some(failure(id, -32602, "Unknown reference")),
        }
    }

    /// What a server said.
    async fn server_said(&self, idx: usize, mut message: Value) {
        let namespace = self.members[idx].namespace.clone();
        let mut answered = None;
        if let Kind::Response(id) = kind(&message) {
            answered = self.members[idx]
                .waiting
                .lock()
                .ok()
                .and_then(|mut waiting| waiting.remove(&id_key(&id)));
        }
        self.observer.frame(
            &namespace,
            &self.id,
            Traffic {
                direction: Direction::Inbound,
                frame: &message,
                method: answered.as_ref().map(|w| w.method.as_str()),
                elapsed: answered.as_ref().map(|w| w.at.elapsed()),
            },
        );
        match kind(&message) {
            Kind::Response(id) => match answered {
                Some(Waiting {
                    waiter: Waiter::Internal(tx),
                    ..
                }) => {
                    let _ = tx.send(message);
                }
                Some(Waiting {
                    waiter: Waiter::Agent,
                    method,
                    ..
                }) => {
                    if method == "tools/call"
                        && let Some(result) = message.get("result")
                        && let Ok(mut owners) = self.owners.lock()
                    {
                        for uri in merge::uris_in_result(result) {
                            owners.link(&namespace, &uri);
                        }
                    }
                    let key = id_key(&id);
                    if let Ok(mut inflight) = self.inflight.lock() {
                        inflight.remove(&key);
                    }
                    self.streams.answer(&key, message).await;
                }
                None => {}
            },
            // A request of the server's own: the agent answers it, and the
            // id says whose it was.
            Kind::Request(id) => {
                message["id"] = Value::String(format!("coco-{idx}-{}", id_key(&id)));
                self.streams.unsolicited(message).await;
            }
            Kind::Notification | Kind::Other => {
                // A server giving up on a request of its own names it by the
                // id it used, which the agent knows it by in the form it was
                // sent on.
                if message["method"] == "notifications/cancelled"
                    && let Some(request) = message["params"].get("requestId").cloned()
                {
                    message["params"]["requestId"] =
                        Value::String(format!("coco-{idx}-{}", id_key(&request)));
                }
                self.streams.unsolicited(message).await;
            }
        }
    }

    /// Read what a server says until it stops.
    async fn pump(self: Arc<Self>, idx: usize, mut incoming: mpsc::Receiver<Value>) {
        loop {
            let message = tokio::select! {
                () = self.ended.cancelled() => return,
                message = incoming.recv() => message,
            };
            let Some(message) = message else {
                break;
            };
            self.server_said(idx, message).await;
        }
        self.member_down(idx, "the server ended the session").await;
    }

    /// A server is gone: what waited on it is answered with an error, and
    /// with none left the root is over.
    async fn member_down(&self, idx: usize, reason: &str) {
        let member = &self.members[idx];
        if member.down.swap(true, Ordering::SeqCst) {
            return;
        }
        self.observer.closed(&member.namespace, &self.id, reason);
        if let Ok(mut inflight) = self.inflight.lock() {
            inflight.retain(|_, at| *at != idx);
        }
        let orphaned: Vec<(String, Waiting)> = member
            .waiting
            .lock()
            .map(|mut w| w.drain().collect())
            .unwrap_or_default();
        for (key, waiting) in orphaned {
            if matches!(waiting.waiter, Waiter::Agent) {
                let id: Value = serde_json::from_str(&key).unwrap_or(Value::Null);
                let message = format!("the server `{}` went away", member.namespace);
                self.streams
                    .answer(&key, failure(&id, -32603, &message))
                    .await;
            }
        }
        if self.up().next().is_none() {
            self.close("every server ended its session").await;
        }
    }

    /// The agent answered a request of a server's: back to that server under
    /// the id it used.
    async fn answer_server(&self, message: Value) {
        let Some(id) = message.get("id").and_then(Value::as_str) else {
            return;
        };
        let Some(rest) = id.strip_prefix("coco-") else {
            return;
        };
        let Some((idx, original)) = rest.split_once('-') else {
            return;
        };
        let (Ok(idx), Ok(original)) = (
            idx.parse::<usize>(),
            serde_json::from_str::<Value>(original),
        ) else {
            return;
        };
        if idx >= self.members.len() || self.members[idx].down.load(Ordering::SeqCst) {
            return;
        }
        let mut message = message;
        message["id"] = original;
        let _ = self.send(idx, message).await;
    }

    /// What the agent sent that is no request: cancellations go to the server
    /// that has the request, the rest to every server.
    async fn agent_said(&self, message: Value) {
        match kind(&message) {
            Kind::Response(_) => self.answer_server(message).await,
            Kind::Notification => {
                let method = message["method"].as_str().unwrap_or_default();
                match method {
                    // Sent to every server already, at the handshake.
                    "notifications/initialized" => {}
                    "notifications/cancelled" => {
                        let key = message["params"]
                            .get("requestId")
                            .map(id_key)
                            .unwrap_or_default();
                        let idx = self.inflight.lock().ok().and_then(|i| i.get(&key).copied());
                        if let Some(idx) = idx {
                            let _ = self.send(idx, message).await;
                        }
                    }
                    _ => {
                        let up: Vec<usize> = self.up().map(|(idx, _)| idx).collect();
                        for idx in up {
                            let _ = self.send(idx, message.clone()).await;
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

#[async_trait]
impl Session for Aggregate {
    fn id(&self) -> &str {
        &self.id
    }

    fn namespace(&self) -> &str {
        ""
    }

    fn is_closed(&self) -> bool {
        self.ended.is_cancelled()
    }

    fn is_idle(&self, limit: Duration) -> bool {
        self.streams.is_idle(limit)
    }

    async fn post(&self, frame: Frame, _context: &RequestContext) -> Result<Posted, UpstreamError> {
        if self.is_closed() {
            return Err(UpstreamError::Closed);
        }
        self.streams.touch();
        match kind(&frame.value) {
            Kind::Request(id) => {
                let rx = self.streams.register(&id_key(&id));
                let method = frame.value["method"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                let params = frame.value.get("params").cloned().unwrap_or(Value::Null);
                if let Some(me) = self.me.upgrade() {
                    match self.permits.clone().try_acquire_owned() {
                        Ok(permit) => {
                            tokio::spawn(async move {
                                let _place = permit;
                                me.handle(id, method, params).await;
                            });
                        }
                        Err(_) => {
                            tokio::spawn(async move {
                                let busy = failure(&id, -32603, "Too many requests at once");
                                me.streams.answer(&id_key(&id), busy).await;
                            });
                        }
                    }
                }
                Ok(Posted::Stream(rx))
            }
            _ => {
                self.agent_said(frame.value).await;
                Ok(Posted::Accepted)
            }
        }
    }

    fn listen(&self) -> Option<mpsc::Receiver<Value>> {
        self.streams.listen()
    }

    async fn close(&self, reason: &str) {
        if self.ended.is_cancelled() {
            return;
        }
        self.ended.cancel();
        for member in &self.members {
            member.upstream.close().await;
            if !member.down.swap(true, Ordering::SeqCst) {
                self.observer.closed(&member.namespace, &self.id, reason);
            }
        }
        self.streams.clear();
    }
}

/// Open one server and shake hands with it. A server that cannot be opened
/// still shows in its log: that an agent reached for it, why it could not be
/// opened, and that it is gone.
async fn open_member(
    namespace: String,
    connector: Arc<dyn Connector>,
    params: &Value,
    session: &str,
    info: Option<ClientInfo>,
    observer: Arc<dyn Observer>,
) -> Result<(Member, mpsc::Receiver<Value>), String> {
    observer.opened(&namespace, session);
    if let Some(info) = info {
        observer.client(&namespace, session, info);
    }
    let unopened = |why: &str| -> String {
        observer.stderr(
            &namespace,
            session,
            &format!("the gateway could not open this server for the root: {why}"),
        );
        observer.closed(&namespace, session, "could not be opened");
        why.to_owned()
    };
    let stderr: StderrSink = {
        let (observer, namespace, session) =
            (observer.clone(), namespace.clone(), session.to_owned());
        Arc::new(move |line| observer.stderr(&namespace, &session, &line))
    };
    let Opened {
        upstream,
        mut incoming,
    } = match tokio::time::timeout(OPEN_TIMEOUT, connector.open(stderr)).await {
        Ok(Ok(opened)) => opened,
        Ok(Err(e)) => return Err(unopened(&e.to_string())),
        Err(_) => return Err(unopened("it did not start in time")),
    };
    let report = |direction, frame: &Value, method, elapsed| {
        observer.frame(
            &namespace,
            session,
            Traffic {
                direction,
                frame,
                method,
                elapsed,
            },
        );
    };
    let init = request(&json!(INIT), "initialize", params);
    report(Direction::Outbound, &init, None, None);
    let started = Instant::now();
    let handshake = async {
        upstream
            .send(&frame_of(init.clone()), &RequestContext::default())
            .await
            .map_err(|e| e.to_string())?;
        while let Some(message) = incoming.recv().await {
            if let Kind::Response(id) = kind(&message)
                && id == json!(INIT)
            {
                report(
                    Direction::Inbound,
                    &message,
                    Some("initialize"),
                    Some(started.elapsed()),
                );
                return match message.get("error") {
                    Some(error) => Err(error["message"]
                        .as_str()
                        .unwrap_or("it refused the handshake")
                        .to_owned()),
                    None => Ok(message["result"].clone()),
                };
            }
        }
        Err("it ended before answering".to_owned())
    };
    let result = match tokio::time::timeout(OPEN_TIMEOUT, handshake).await {
        Ok(Ok(result)) => result,
        Ok(Err(why)) => {
            upstream.close().await;
            return Err(unopened(&why));
        }
        Err(_) => {
            upstream.close().await;
            return Err(unopened("it did not answer in time"));
        }
    };
    let initialized = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    report(Direction::Outbound, &initialized, None, None);
    if let Err(e) = upstream
        .send(&frame_of(initialized), &RequestContext::default())
        .await
    {
        upstream.close().await;
        return Err(unopened(&e.to_string()));
    }
    let version = result
        .get("protocolVersion")
        .and_then(Value::as_str)
        .or_else(|| params.get("protocolVersion").and_then(Value::as_str))
        .unwrap_or("2025-11-25")
        .to_owned();
    let member = Member {
        namespace: namespace.clone(),
        upstream,
        capabilities: result.get("capabilities").cloned().unwrap_or(Value::Null),
        version,
        waiting: Mutex::new(HashMap::new()),
        down: AtomicBool::new(false),
    };
    Ok((member, incoming))
}
