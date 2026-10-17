//! The gateway as the app runs it: its settings, the servers it answers for,
//! who is connected, and the traffic it reports, which lands in the log of
//! the server it went to.
//!
//! The relay itself is `mcp-gateway`. This is what the window adds to it:
//! the choices that are remembered, the credentials each server is called
//! with (from the keyring, never typed here), and the model that starts and
//! stops it.

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui_kit::Context;
use mcp_auth::SecretStore;
use mcp_core::{AuthRef, Event, EventKind, ServerSpec};
use mcp_gateway::{
    Authorizer, ClientInfo, Connector, HttpConnector, NoAuth, Observer, RelayServer,
    StdioConnector, Traffic, valid_namespace,
};
use mcp_store::{ServerRecord, Store};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::bridge::drain_ready;
use crate::state::{AppState, Dir, LogRow, incoming};

/// The port the gateway listens on until another is chosen.
pub const DEFAULT_PORT: u16 = 7341;
/// The lowest port that can be chosen: below it a program needs privileges.
pub const MIN_PORT: u16 = 1024;
/// What the agent's name is until it says who it is.
const UNNAMED: &str = "agent";

const ON_SETTING: &str = "gateway.on";
const PORT_SETTING: &str = "gateway.port";
const SERVERS_SETTING: &str = "gateway.servers";
/// Where the access token is kept, in the secret store.
const TOKEN_KEY: &str = "gateway-token";

/// A server's part in the gateway.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Included {
    /// Whether the gateway answers for it.
    pub on: bool,
    /// The path it answers on.
    pub namespace: String,
}

/// What is remembered about the gateway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// Whether it runs while the app is open.
    pub on: bool,
    /// The port it listens on.
    pub port: u16,
    /// Choices made for a server, by its id. A server with none is included
    /// under a namespace made from its name.
    pub servers: BTreeMap<String, Included>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            on: false,
            port: DEFAULT_PORT,
            servers: BTreeMap::new(),
        }
    }
}

impl Settings {
    /// Read the settings, taking the default for what is missing or unreadable.
    pub fn load(store: &Store) -> Result<Self, String> {
        let read = |e: mcp_store::Error| e.to_string();
        Ok(Self {
            on: store
                .get_setting(ON_SETTING)
                .map_err(read)?
                .unwrap_or(false),
            port: store
                .get_setting(PORT_SETTING)
                .map_err(read)?
                .filter(|p| *p >= MIN_PORT)
                .unwrap_or(DEFAULT_PORT),
            servers: store
                .get_setting(SERVERS_SETTING)
                .map_err(read)?
                .unwrap_or_default(),
        })
    }

    /// Write the settings.
    fn save(&self, store: &Store) -> Result<(), String> {
        let wrote = |e: mcp_store::Error| e.to_string();
        store.set_setting(ON_SETTING, &self.on).map_err(wrote)?;
        store.set_setting(PORT_SETTING, &self.port).map_err(wrote)?;
        store
            .set_setting(SERVERS_SETTING, &self.servers)
            .map_err(wrote)
    }

    /// Each server's part, in the order the servers are listed: what was
    /// chosen for it, or inclusion under a namespace made from its name. A
    /// namespace that is not valid, or that an earlier server holds, is
    /// replaced by a free one, so two servers never share a path.
    pub fn plan(&self, servers: &[ServerRecord]) -> Vec<(String, Included)> {
        let mut taken: Vec<String> = Vec::new();
        let mut plan = Vec::new();
        for server in servers {
            let chosen = self.servers.get(&server.id);
            let wanted = chosen
                .map(|c| c.namespace.clone())
                .filter(|n| valid_namespace(n))
                .unwrap_or_else(|| default_namespace(&server.name));
            let namespace = unique_namespace(&wanted, |n| taken.iter().any(|t| t == n));
            taken.push(namespace.clone());
            plan.push((
                server.id.clone(),
                Included {
                    on: chosen.is_none_or(|c| c.on),
                    namespace,
                },
            ));
        }
        plan
    }
}

/// A namespace made from a server's name: lower case, letters and digits and
/// `_`, other runs becoming a single `-`.
pub fn default_namespace(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-');
    let out: String = out.chars().take(64).collect();
    if out.is_empty() { "server".into() } else { out }
}

/// `base`, or `base-2`, `base-3`… until `taken` says it is free.
pub fn unique_namespace(base: &str, taken: impl Fn(&str) -> bool) -> String {
    if !taken(base) {
        return base.to_owned();
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|candidate| !taken(candidate))
        .unwrap_or_else(|| base.to_owned())
}

/// Whether the gateway is up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Not running.
    Off,
    /// Starting.
    Starting,
    /// Listening here.
    Running(SocketAddr),
    /// It could not start or keep running, and why.
    Failed(String),
}

/// An agent connected through the gateway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agent {
    /// The server it reaches.
    pub server_id: String,
    /// What it says it is called.
    pub name: String,
    /// Its version, when it said.
    pub version: Option<String>,
    /// Whether the log says it connected yet: that waits for its first row,
    /// by when it has said who it is.
    announced: bool,
}

/// The gateway in the model.
pub struct GatewayState {
    /// What is remembered.
    pub settings: Settings,
    /// Whether it is up.
    pub status: Status,
    /// Who is connected, by connection.
    pub agents: BTreeMap<String, Agent>,
    token: Option<String>,
    handle: Option<Arc<mcp_gateway::Gateway>>,
    /// How each path is opened, kept while its server is unchanged so that
    /// changing another server does not end the agents connected to this one.
    connectors: HashMap<String, (ServerSpec, Arc<dyn Connector>)>,
    /// What the running gateway was last told, to see when it needs telling.
    applied: Vec<(String, ServerSpec)>,
    /// Counts starts, so the answer of one that was replaced is dropped.
    run: u64,
}

impl std::fmt::Debug for GatewayState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The token is a credential: never printed.
        f.debug_struct("GatewayState")
            .field("settings", &self.settings)
            .field("status", &self.status)
            .field("agents", &self.agents.len())
            .finish_non_exhaustive()
    }
}

impl Default for GatewayState {
    fn default() -> Self {
        Self {
            settings: Settings::default(),
            status: Status::Off,
            agents: BTreeMap::new(),
            token: None,
            handle: None,
            connectors: HashMap::new(),
            applied: Vec::new(),
            run: 0,
        }
    }
}

impl GatewayState {
    /// The access token, once the gateway has made or read it.
    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    /// The end of the token, enough to tell one from another.
    pub fn token_hint(&self) -> Option<String> {
        let token = self.token.as_deref()?;
        let tail: String = token
            .chars()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        Some(format!("coco_••••{tail}"))
    }

    /// The URL an agent reaches `namespace` at, when the gateway is up.
    pub fn url(&self, namespace: &str) -> Option<String> {
        match &self.status {
            Status::Running(addr) => Some(format!("http://{addr}/{namespace}")),
            _ => None,
        }
    }
}

// ------------------------------------------------------------ credentials

/// A server's bearer token, read from the keyring for each request.
struct KeyringBearer {
    secrets: Arc<dyn SecretStore>,
    key: String,
}

#[async_trait]
impl Authorizer for KeyringBearer {
    async fn authorization(&self) -> Result<Option<String>, String> {
        let (secrets, key) = (self.secrets.clone(), self.key.clone());
        let token = tokio::task::spawn_blocking(move || secrets.get(&key))
            .await
            .map_err(|e| e.to_string())??;
        match token {
            Some(token) => Ok(Some(format!("Bearer {token}"))),
            None => Err("no token is stored for this server; edit it in Coco and enter one".into()),
        }
    }
}

/// A server's OAuth login as the app stored it. The gateway never signs
/// anyone in: without stored credentials it says to do that in the app.
struct StoredOAuth(mcp_auth::StoredToken);

#[async_trait]
impl Authorizer for StoredOAuth {
    async fn authorization(&self) -> Result<Option<String>, String> {
        match self.0.access_token().await {
            Ok(token) => Ok(Some(format!("Bearer {token}"))),
            Err(mcp_auth::Error::MissingSecret(_)) => {
                Err("this server is not signed in: connect it in Coco first".into())
            }
            Err(e) => Err(e.to_string()),
        }
    }
}

/// How to open `record` for an agent, with the credentials it is called with.
fn connector(
    record: &ServerRecord,
    secrets: &Arc<dyn SecretStore>,
) -> Result<Arc<dyn Connector>, String> {
    match &record.spec {
        ServerSpec::Stdio {
            command,
            args,
            env,
            cwd,
        } => Ok(Arc::new(StdioConnector {
            command: command.clone(),
            args: args.clone(),
            env: env.clone(),
            cwd: cwd.clone(),
        })),
        ServerSpec::Http { url, headers, auth } => {
            let authorizer: Arc<dyn Authorizer> =
                match auth {
                    AuthRef::None => Arc::new(NoAuth),
                    AuthRef::Bearer { keyring_id } => Arc::new(KeyringBearer {
                        secrets: secrets.clone(),
                        key: keyring_id.clone(),
                    }),
                    AuthRef::OAuth { keyring_id } => Arc::new(StoredOAuth(
                        mcp_auth::StoredToken::new(url, secrets.clone(), keyring_id),
                    )),
                };
            let client = mcp_core::transport::http_client_builder(url)
                .build()
                .map_err(|e| format!("{}: {e}", record.name))?;
            Ok(Arc::new(HttpConnector {
                url: url.clone(),
                headers: headers
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                authorizer,
                client,
            }))
        }
    }
}

/// The servers the gateway answers for, as `settings` plans them. A path
/// whose server is as it was keeps the connector it had in `cache`.
fn relay_servers(
    settings: &Settings,
    servers: &[ServerRecord],
    secrets: &Arc<dyn SecretStore>,
    cache: &mut HashMap<String, (ServerSpec, Arc<dyn Connector>)>,
) -> Result<Vec<RelayServer>, String> {
    let mut out = Vec::new();
    let mut kept = HashMap::new();
    for (record, (_, included)) in servers.iter().zip(settings.plan(servers)) {
        if !included.on {
            continue;
        }
        let connector = match cache.remove(&included.namespace) {
            Some((spec, connector)) if spec == record.spec => connector,
            _ => connector(record, secrets)?,
        };
        kept.insert(
            included.namespace.clone(),
            (record.spec.clone(), connector.clone()),
        );
        out.push(RelayServer {
            namespace: included.namespace,
            connector,
        });
    }
    *cache = kept;
    Ok(out)
}

/// What the gateway answers for, to compare with what it was told.
fn plan_of(settings: &Settings, servers: &[ServerRecord]) -> Vec<(String, ServerSpec)> {
    servers
        .iter()
        .zip(settings.plan(servers))
        .filter(|(_, (_, included))| included.on)
        .map(|(record, (_, included))| (included.namespace, record.spec.clone()))
        .collect()
}

/// The access token: the one in the keyring, or a new one kept there.
fn token_from(secrets: &Arc<dyn SecretStore>) -> Result<String, String> {
    if let Some(token) = secrets.get(TOKEN_KEY)? {
        return Ok(token);
    }
    let token = new_token();
    secrets.set(TOKEN_KEY, &token)?;
    Ok(token)
}

fn new_token() -> String {
    format!(
        "coco_{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

// ------------------------------------------------------------ the traffic

/// What the gateway reports, moved to the model's thread.
#[derive(Debug)]
enum Relayed {
    Opened {
        namespace: String,
        session: String,
    },
    Client {
        session: String,
        info: ClientInfo,
    },
    Frame {
        session: String,
        at: time::OffsetDateTime,
        kind: EventKind,
    },
    Stderr {
        session: String,
        at: time::OffsetDateTime,
        line: String,
    },
    Closed {
        session: String,
        at: time::OffsetDateTime,
        reason: String,
    },
}

/// Hands what the gateway sees to the model, a message at a time.
struct Tap(mpsc::UnboundedSender<Relayed>);

impl Observer for Tap {
    fn opened(&self, namespace: &str, session: &str) {
        let _ = self.0.unbounded_send(Relayed::Opened {
            namespace: namespace.to_owned(),
            session: session.to_owned(),
        });
    }

    fn client(&self, _: &str, session: &str, info: ClientInfo) {
        let _ = self.0.unbounded_send(Relayed::Client {
            session: session.to_owned(),
            info,
        });
    }

    fn frame(&self, _: &str, session: &str, traffic: Traffic<'_>) {
        if let Some(kind) = traffic.event() {
            let _ = self.0.unbounded_send(Relayed::Frame {
                session: session.to_owned(),
                at: time::OffsetDateTime::now_utc(),
                kind,
            });
        }
    }

    fn stderr(&self, _: &str, session: &str, line: &str) {
        let _ = self.0.unbounded_send(Relayed::Stderr {
            session: session.to_owned(),
            at: time::OffsetDateTime::now_utc(),
            line: line.to_owned(),
        });
    }

    fn closed(&self, _: &str, session: &str, reason: &str) {
        let _ = self.0.unbounded_send(Relayed::Closed {
            session: session.to_owned(),
            at: time::OffsetDateTime::now_utc(),
            reason: reason.to_owned(),
        });
    }
}

impl AppState {
    /// Read the gateway's settings; called with the other settings at start.
    pub(crate) fn load_gateway(&mut self, store: &Store) {
        let settings = Settings::load(store);
        if let Some(settings) = self.note_write("gateway settings could not be read", settings) {
            self.gateway.settings = settings;
        }
    }

    /// Whether the gateway should be started when the app opens.
    pub fn gateway_wanted(&self) -> bool {
        self.gateway.settings.on
    }

    /// Remember the settings.
    fn save_gateway(&mut self, cx: &mut Context<Self>) {
        let Ok(Some(store)) = self.writable_store() else {
            return;
        };
        let settings = self.gateway.settings.clone();
        self.in_background(
            move || settings.save(&store),
            |state, saved, _| {
                let saved = saved.unwrap_or_else(|| Err("gateway settings task failed".into()));
                state.note_write("gateway settings were not saved", saved);
            },
            cx,
        );
    }

    /// Turn the gateway on or off.
    pub fn gateway_set_on(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.gateway.settings.on == on {
            return;
        }
        self.gateway.settings.on = on;
        self.save_gateway(cx);
        if on {
            self.gateway_start(cx);
        } else {
            self.gateway_stop(cx);
        }
        self.changed(cx);
    }

    /// Listen on `port` from now on. A port below [`MIN_PORT`] is refused.
    pub fn gateway_set_port(&mut self, port: u16, cx: &mut Context<Self>) -> Result<(), String> {
        if port < MIN_PORT {
            return Err(format!("choose a port from {MIN_PORT} up"));
        }
        if self.gateway.settings.port == port {
            return Ok(());
        }
        self.gateway.settings.port = port;
        self.save_gateway(cx);
        if self.gateway.settings.on {
            self.gateway_start(cx);
        }
        self.changed(cx);
        Ok(())
    }

    /// Include server `id` or not, and/or name its path. A namespace that is
    /// not valid or that another server holds is refused.
    pub fn gateway_set_server(
        &mut self,
        id: &str,
        on: Option<bool>,
        namespace: Option<&str>,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let records: Vec<ServerRecord> = self.servers.iter().map(|s| s.record.clone()).collect();
        let plan = self.gateway.settings.plan(&records);
        let Some((_, current)) = plan.iter().find(|(sid, _)| sid == id) else {
            return Err("no such server".into());
        };
        let mut next = current.clone();
        if let Some(on) = on {
            next.on = on;
        }
        if let Some(namespace) = namespace {
            if !valid_namespace(namespace) {
                return Err("use letters, digits, - and _ (up to 64)".into());
            }
            if plan
                .iter()
                .any(|(sid, p)| sid != id && p.namespace == namespace)
            {
                return Err("another server has this path".into());
            }
            namespace.clone_into(&mut next.namespace);
        }
        if *current == next {
            return Ok(());
        }
        self.gateway.settings.servers.insert(id.to_owned(), next);
        self.save_gateway(cx);
        self.gateway_refresh(cx);
        self.changed(cx);
        Ok(())
    }

    /// Replace the access token. Agents using the old one are turned away.
    pub fn gateway_regenerate_token(&mut self, cx: &mut Context<Self>) {
        let secrets = self.secrets.clone();
        let token = new_token();
        let kept = token.clone();
        self.in_background(
            move || secrets.set(TOKEN_KEY, &kept),
            move |state, saved, cx| {
                let saved = saved.unwrap_or_else(|| Err("keyring task failed".into()));
                if state
                    .note_write("the gateway's access token was not saved", saved)
                    .is_some()
                {
                    state.gateway.token = Some(token);
                    if state.gateway.settings.on {
                        state.gateway_start(cx);
                    }
                    state.changed(cx);
                }
            },
            cx,
        );
    }

    /// Tell a running gateway when the servers it answers for changed: one
    /// edited, removed or added, or its path or inclusion chosen.
    pub fn gateway_sync(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.gateway.status, Status::Running(_)) {
            return;
        }
        let records: Vec<ServerRecord> = self.servers.iter().map(|s| s.record.clone()).collect();
        if plan_of(&self.gateway.settings, &records) != self.gateway.applied {
            self.gateway_refresh(cx);
            self.changed(cx);
        }
    }

    /// Change the servers a running gateway answers for.
    fn gateway_refresh(&mut self, cx: &mut Context<Self>) {
        let (Some(handle), Some(bridge)) = (self.gateway.handle.clone(), self.bridge.clone())
        else {
            return;
        };
        let records: Vec<ServerRecord> = self.servers.iter().map(|s| s.record.clone()).collect();
        let secrets = self.secrets.clone();
        let settings = self.gateway.settings.clone();
        match relay_servers(&settings, &records, &secrets, &mut self.gateway.connectors) {
            Ok(servers) => {
                self.gateway.applied = plan_of(&settings, &records);
                let task = bridge.run(async move { handle.set_servers(servers).await });
                cx.spawn(async move |_, _| {
                    task.await;
                })
                .detach();
            }
            Err(e) => self.gateway.status = Status::Failed(e),
        }
    }

    /// Stop the gateway; every agent is disconnected.
    pub fn gateway_stop(&mut self, cx: &mut Context<Self>) {
        self.gateway.run += 1;
        self.gateway.status = Status::Off;
        self.gateway.agents.clear();
        if let (Some(handle), Some(bridge)) = (self.gateway.handle.take(), self.bridge.clone()) {
            let task = bridge.run(async move { handle.stop().await });
            cx.spawn(async move |_, _| {
                task.await;
            })
            .detach();
        }
    }

    /// Start the gateway on the chosen port, replacing one that is running.
    pub fn gateway_start(&mut self, cx: &mut Context<Self>) {
        self.gateway_stop(cx);
        let Some(bridge) = self.bridge.clone() else {
            return;
        };
        let records: Vec<ServerRecord> = self.servers.iter().map(|s| s.record.clone()).collect();
        // A new gateway opens everything afresh, a login made since included.
        self.gateway.connectors.clear();
        let secrets = self.secrets.clone();
        let settings = self.gateway.settings.clone();
        let servers =
            match relay_servers(&settings, &records, &secrets, &mut self.gateway.connectors) {
                Ok(servers) => servers,
                Err(e) => {
                    self.gateway.status = Status::Failed(e);
                    return;
                }
            };
        self.gateway.applied = plan_of(&settings, &records);
        self.gateway.status = Status::Starting;
        let run = self.gateway.run;
        let (port, secrets) = (self.gateway.settings.port, self.secrets.clone());
        let (tx, mut events) = mpsc::unbounded();
        let task = bridge.run(async move {
            let kept = secrets.clone();
            let token = tokio::task::spawn_blocking(move || token_from(&kept))
                .await
                .map_err(|e| e.to_string())??;
            let gateway =
                mcp_gateway::Gateway::start(port, token.clone(), servers, Arc::new(Tap(tx)))
                    .await
                    .map_err(|e| match e.kind() {
                        std::io::ErrorKind::AddrInUse => {
                            format!("port {port} is in use: choose another")
                        }
                        _ => format!("cannot listen on port {port}: {e}"),
                    })?;
            Ok::<_, String>((gateway, token))
        });
        cx.spawn(async move |this, cx| {
            let started = task
                .await
                .unwrap_or_else(|| Err("the gateway task failed".into()));
            let Ok((gateway, token)) = started.map_err(|e| {
                let _ = this.update(cx, |state, cx| {
                    if state.gateway.run == run {
                        state.gateway.status = Status::Failed(e);
                        state.changed(cx);
                    }
                });
            }) else {
                return;
            };
            let gateway = Arc::new(gateway);
            let kept = gateway.clone();
            let live = this
                .update(cx, |state, cx| {
                    if state.gateway.run != run {
                        return false;
                    }
                    state.gateway.status = Status::Running(kept.addr());
                    state.gateway.token = Some(token);
                    state.gateway.handle = Some(kept);
                    state.changed(cx);
                    true
                })
                .unwrap_or(false);
            if !live {
                // Replaced or stopped while it was starting.
                if let Some(bridge) = this.update(cx, |s, _| s.bridge.clone()).ok().flatten() {
                    let _ = bridge.run(async move { gateway.stop().await }).await;
                }
                return;
            }
            while let Some(first) = events.next().await {
                let batch = drain_ready(first, &mut events);
                let applied = this.update(cx, |state, cx| {
                    state.relayed(run, batch, cx);
                });
                if applied.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// File what the gateway reported: connections, and each message in the
    /// log of the server it went to.
    fn relayed(&mut self, run: u64, batch: Vec<Relayed>, cx: &mut Context<Self>) {
        if self.gateway.run != run {
            return;
        }
        for item in batch {
            match item {
                Relayed::Opened { namespace, session } => {
                    let records: Vec<ServerRecord> =
                        self.servers.iter().map(|s| s.record.clone()).collect();
                    let Some((server_id, _)) = self
                        .gateway
                        .settings
                        .plan(&records)
                        .into_iter()
                        .find(|(_, p)| p.namespace == namespace)
                    else {
                        continue;
                    };
                    self.gateway.agents.insert(
                        session,
                        Agent {
                            server_id,
                            name: UNNAMED.into(),
                            version: None,
                            announced: false,
                        },
                    );
                }
                Relayed::Client { session, info } => {
                    if let Some(agent) = self.gateway.agents.get_mut(&session) {
                        agent.name = info.name;
                        agent.version = info.version;
                    }
                }
                Relayed::Frame { session, at, kind } => {
                    let row = incoming(Event { seq: 0, at, kind }).row;
                    if let Some(row) = row {
                        self.relay_row(&session, row);
                    }
                }
                Relayed::Stderr { session, at, line } => {
                    let row = incoming(Event {
                        seq: 0,
                        at,
                        kind: EventKind::Stderr { line },
                    })
                    .row;
                    if let Some(row) = row {
                        self.relay_row(&session, row);
                    }
                }
                Relayed::Closed {
                    session,
                    at,
                    reason,
                } => {
                    self.relay_note(&session, at, "disconnected", &reason);
                    self.gateway.agents.remove(&session);
                }
            }
        }
        self.changed(cx);
    }

    /// A note in the log of the server `session` reaches.
    fn relay_note(&mut self, session: &str, at: time::OffsetDateTime, what: &str, text: &str) {
        let row = LogRow::new(
            at,
            Dir::Note,
            format!("relay {what}"),
            Value::String(text.into()),
            false,
        );
        self.relay_row(session, row);
    }

    /// File `row` under the server `session` reaches, naming the agent. The
    /// first row of a connection is preceded by the note that it connected.
    fn relay_row(&mut self, session: &str, mut row: LogRow) {
        let Some(agent) = self.gateway.agents.get_mut(session) else {
            return;
        };
        let first = !std::mem::replace(&mut agent.announced, true);
        let who = match &agent.version {
            Some(version) => format!("{} {version}", agent.name),
            None => agent.name.clone(),
        };
        row.agent = Some(agent.name.clone());
        let id = agent.server_id.clone();
        let at = row.at;
        let Some(pos) = self.servers.iter().position(|s| s.record.id == id) else {
            return;
        };
        if first {
            let mut note = LogRow::new(
                at,
                Dir::Note,
                "relay connected".into(),
                Value::String(format!("{who} connected through the gateway")),
                false,
            );
            note.agent = row.agent.clone();
            self.append_log(pos, note);
        }
        self.append_log(pos, row);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str, name: &str) -> ServerRecord {
        serde_json::from_value(serde_json::json!({
            "id": id, "name": name,
            "spec": {"transport": "stdio", "command": "x", "args": [], "env": {}},
            "policy": serde_json::to_value(mcp_core::ServerRequestPolicy::default()).unwrap(),
        }))
        .unwrap()
    }

    #[test]
    fn a_namespace_is_made_from_a_name() {
        assert_eq!(default_namespace("Weather API"), "weather-api");
        assert_eq!(default_namespace("  my_server!! v2 "), "my_server-v2");
        assert_eq!(default_namespace("日本語"), "server");
        assert_eq!(default_namespace(&"a".repeat(100)).len(), 64);
        assert!(valid_namespace(&default_namespace("Weather API")));
        assert_eq!(unique_namespace("a", |n| n == "a" || n == "a-2"), "a-3");
    }

    #[test]
    fn servers_are_planned_into_free_paths() {
        let servers = [
            record("1", "Search"),
            record("2", "search"),
            record("3", "Files"),
        ];
        let mut settings = Settings::default();
        let plan = settings.plan(&servers);
        let paths: Vec<&str> = plan.iter().map(|(_, p)| p.namespace.as_str()).collect();
        assert_eq!(paths, ["search", "search-2", "files"]);
        assert!(
            plan.iter().all(|(_, p)| p.on),
            "included until switched off"
        );

        // What was chosen is kept; a clash or an invalid path is not.
        settings.servers.insert(
            "3".into(),
            Included {
                on: false,
                namespace: "search".into(),
            },
        );
        settings.servers.insert(
            "2".into(),
            Included {
                on: true,
                namespace: "not valid!".into(),
            },
        );
        let plan = settings.plan(&servers);
        let paths: Vec<&str> = plan.iter().map(|(_, p)| p.namespace.as_str()).collect();
        assert_eq!(paths, ["search", "search-2", "search-3"]);
        assert!(!plan[2].1.on);
    }

    #[test]
    fn settings_are_read_back() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(Settings::load(&store).unwrap(), Settings::default());
        let mut settings = Settings {
            on: true,
            port: 7400,
            servers: BTreeMap::new(),
        };
        settings.servers.insert(
            "1".into(),
            Included {
                on: false,
                namespace: "x".into(),
            },
        );
        settings.save(&store).unwrap();
        assert_eq!(Settings::load(&store).unwrap(), settings);
        // A port that cannot be chosen is not read back either.
        store.set_setting(PORT_SETTING, &80u16).unwrap();
        assert_eq!(Settings::load(&store).unwrap().port, DEFAULT_PORT);
    }

    #[test]
    fn the_token_is_never_printed() {
        let state = GatewayState {
            token: Some("coco_secretsecretsecret1234".into()),
            ..GatewayState::default()
        };
        assert_eq!(state.token_hint().as_deref(), Some("coco_••••1234"));
        assert!(!format!("{state:?}").contains("secret"));
        assert_eq!(new_token().len(), "coco_".len() + 64);
    }
}
