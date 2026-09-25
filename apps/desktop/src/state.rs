//! `AppState`: the one entity every view reads. Sessions, selection, the
//! log rows and the current screen live here; views are thin renderers over
//! it and mutate it through its methods.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::time::Duration;

use futures::StreamExt as _;
use futures::channel::oneshot;
use gpui_kit::{Context, EventEmitter};
use mcp_core::{
    ConnectionState, Direction, Event, EventKind, EventSink, ListFailure, ListKind, Prompt,
    ProtocolMode, Resource, Root, ServerSpec, Session, SessionOptions, Snapshot, Tool, list_method,
};
use mcp_store::{CallRecord, ServerRecord, Store};
use serde_json::Value;

use crate::bridge::{Bridge, drain_ready};
use crate::calls::{Progress, Responses};
use crate::explain::explain;
use crate::features::{Feature, Features};
use crate::persistence::{
    self, Persistence, SavedServer, Saving, Unsaved, compare_and_store, refused,
};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

/// Which list the middle column shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    /// `tools/list`.
    Tools,
    /// `resources/list` (+ templates).
    Resources,
    /// `prompts/list`.
    Prompts,
    /// Recorded calls of the selected server.
    History,
    /// What the server said about itself when it connected, the lists it
    /// could not deliver, and the roots offered to it.
    Server,
}

impl From<ListKind> for Mode {
    fn from(kind: ListKind) -> Self {
        match kind {
            ListKind::Tools => Self::Tools,
            ListKind::Resources => Self::Resources,
            ListKind::Prompts => Self::Prompts,
        }
    }
}

impl Mode {
    /// All modes in sidebar order.
    pub const ALL: [Mode; 5] = [
        Mode::Tools,
        Mode::Resources,
        Mode::Prompts,
        Mode::History,
        Mode::Server,
    ];

    /// Sidebar label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Tools => "Tools",
            Self::Resources => "Resources",
            Self::Prompts => "Prompts",
            Self::History => "History",
            Self::Server => "Server",
        }
    }

    /// The list requests whose items this mode shows.
    pub fn list_methods(self) -> &'static [&'static str] {
        match self {
            Self::Tools => &[list_method::TOOLS],
            Self::Resources => &[list_method::RESOURCES, list_method::RESOURCE_TEMPLATES],
            Self::Prompts => &[list_method::PROMPTS],
            Self::History | Self::Server => &[],
        }
    }

    /// Filter placeholder.
    pub fn placeholder(self) -> &'static str {
        match self {
            Self::Tools => "Filter tools",
            Self::Resources => "Filter resources",
            Self::Prompts => "Filter prompts",
            Self::History => "Filter calls by content",
            Self::Server => "Filter sections",
        }
    }
}

/// Connection status of one server, as shown by the sidebar dot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Not connected.
    Off,
    /// `initialize` in flight.
    Connecting,
    /// Ready.
    Connected,
    /// Failed or dropped; the text is shown in the status bar.
    Error(String),
}

/// Direction column of a log row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    /// Client to server (`→`).
    Out,
    /// Server to client (`←`).
    In,
    /// Notification or note (`·`).
    Note,
}

impl Dir {
    /// The glyph of the arrow column, which a copied line repeats.
    pub fn arrow(self) -> &'static str {
        match self {
            Self::Out => "→",
            Self::In => "←",
            Self::Note => "·",
        }
    }
}

/// One row of the log drawer.
#[derive(Debug, Clone)]
pub struct LogRow {
    /// When the event was observed. Kept alongside `time` because the
    /// drawer shows a local clock while an export writes RFC 3339.
    pub at: time::OffsetDateTime,
    /// `HH:MM:SS.mmm`.
    pub time: String,
    /// Arrow column.
    pub dir: Dir,
    /// Method or label.
    pub method: String,
    /// One-line body preview.
    pub body: String,
    /// Size of the payload the event carried, `212 B` / `2.1 KB`.
    pub size: String,
    /// Full payload for the expanded view, or the marker that stands in for
    /// one over [`LOG_PAYLOAD_LIMIT`].
    pub payload: Value,
    /// Which JSON-RPC frame the row is, for rebuilding the wire message.
    /// `None` for a row that was never one (stderr, a state change).
    pub frame: Option<mcp_exchange::Frame>,
    /// The frame's JSON-RPC id, when it had one.
    pub id: Option<Value>,
    /// Whether this row is a JSON-RPC error, a failure, or a server log
    /// message at `error` or above.
    pub is_error: bool,
    /// The level of a server log message (`notifications/message`).
    pub level: Option<String>,
    /// Whether this row marks where a new session starts; the rows before it
    /// belong to the connection that ended.
    pub session_break: bool,
    /// Compact bytes of the payload the event carried, when that was over
    /// [`LOG_PAYLOAD_LIMIT`] and `payload` keeps only its head.
    pub truncated: Option<usize>,
    /// Serialized bytes of what the row keeps (its payload or marker as
    /// compact JSON, its body and its method), counted against
    /// [`LOG_BYTE_BUDGET`]. The parsed payload can take a few times that.
    pub(crate) bytes: usize,
    /// The row's id in its server's log, handed out in order by
    /// [`ServerEntry::push_log`] (not the JSON-RPC `id`). An index shifts when
    /// older rows leave; this does not, so folds, list measurements, the copy
    /// menu and the expanded row key on it.
    pub row_id: u64,
}

impl LogRow {
    /// A row carrying `payload`, printed once for both its size and its body.
    ///
    /// A payload over [`LOG_PAYLOAD_LIMIT`] is replaced by a marker holding
    /// its head and its original size, so the row, its copy menu and a log
    /// export all say what was left out.
    fn new(
        at: time::OffsetDateTime,
        dir: Dir,
        method: String,
        payload: Value,
        is_error: bool,
    ) -> Self {
        let (head, total) = mcp_exchange::json_head(&payload, LOG_PAYLOAD_LIMIT);
        let body = preview(&payload, &head);
        let (payload, body, kept, truncated) = if total > LOG_PAYLOAD_LIMIT {
            let note = format!(
                "{} payload, first {} kept",
                size_label(total),
                size_label(head.len())
            );
            let body = format!("truncated {} · {body}", size_label(total));
            let kept = head.len() + note.len();
            let marker = serde_json::json!({
                "truncated": note,
                "originalBytes": total,
                "head": head,
            });
            (marker, body, kept, Some(total))
        } else {
            (payload, body, total, None)
        };
        let bytes = kept + body.len() + method.len();
        Self {
            at,
            time: time_label(at),
            dir,
            method,
            body,
            size: size_label(total),
            payload,
            frame: None,
            id: None,
            is_error,
            level: None,
            session_break: false,
            truncated,
            bytes,
            row_id: 0,
        }
    }

    /// The separator a reconnect leaves between the previous session's rows
    /// and the new ones. It is a note, so an export writes it as one.
    pub fn session_break(at: time::OffsetDateTime) -> Self {
        let payload =
            Value::String("new session · earlier rows are from the previous connection".into());
        Self {
            session_break: true,
            ..Self::new(at, Dir::Note, "session".into(), payload, false)
        }
    }

    /// The message as it went over the wire, for a row that was one.
    pub fn message(&self) -> Option<Value> {
        Some(mcp_exchange::wire_message(
            self.frame?,
            self.id.as_ref(),
            &self.method,
            &self.payload,
        ))
    }
}

/// A server in the sidebar.
#[derive(Debug)]
pub struct ServerEntry {
    /// Persisted record.
    pub record: ServerRecord,
    /// Current status.
    pub status: Status,
    /// Live session when connected.
    pub session: Option<Session>,
    /// Last snapshot. Private so that every change bumps `rev`.
    snapshot: Option<Snapshot>,
    /// Log rows, oldest first. Private so that every row gets its id.
    log: Vec<LogRow>,
    /// Id the next row pushed gets.
    next_log_id: u64,
    /// Sum of the rows' [`LogRow::bytes`], kept under [`LOG_BYTE_BUDGET`].
    log_bytes: usize,
    /// Round-trip of the last call, for the status bar.
    pub last_call_ms: Option<u64>,
    /// How the last snapshot compares with the stored one (set on connect).
    pub diff: Option<mcp_diff::SnapshotDiff>,
    /// Whether the change banner was closed for this connection.
    pub diff_dismissed: bool,
    /// Whether the change banner lists every change.
    pub diff_expanded: bool,
    /// Recorded calls, newest first. Private so that every change bumps `rev`.
    history: Vec<CallRecord>,
    /// Whether the stored calls are in `history` yet.
    history_load: HistoryLoad,
    /// What each listed call's result lays out, in bytes, by call id:
    /// measured off the GPUI thread, where the call was read or answered.
    /// The text the History filter matches, by call id, built once per call
    /// when its row is first listed and dropped with the call.
    search_text: RefCell<HashMap<String, Rc<str>>>,
    /// Resolved once the stored calls are read, for an export waiting on them.
    history_waiters: Vec<oneshot::Sender<()>>,
    /// Bumped whenever `snapshot` or `history` changes, so the middle list
    /// built from them is rebuilt only then.
    rev: u64,
    /// Bumped on every connect so stale futures are ignored.
    generation: u64,
    /// Items a `list_changed` added or altered, marked in the list until
    /// selected. Private so that every change bumps `rev`.
    changed_items: HashSet<(Mode, String)>,
    /// Resources this session is subscribed to. Private so that every change
    /// bumps `rev`.
    subscribed: BTreeSet<String>,
    /// When each resource last reported a change, as `HH:MM:SS.mmm`.
    resource_updates: HashMap<String, String>,
    /// The last subscribe or unsubscribe the server refused: URI and reason.
    pub subscription_error: Option<(String, String)>,
    /// The log level set on the server, sent again after a reconnect.
    pub log_level: Option<String>,
    /// Whether the last connect was turned away for want of authorization.
    pub unauthorized: bool,
    /// The `WWW-Authenticate` challenge of that answer, which the next
    /// authorization starts from instead of guessed metadata locations.
    pub auth_challenge: Option<String>,
    /// The roots offered to this server, once read or answered.
    pub roots: Option<Vec<Root>>,
    /// The snapshots stored for this server, newest first, once read.
    pub snapshots: Option<Vec<mcp_store::SnapshotSummary>>,
    /// The stored snapshots the Server view compares, by id: older, newer.
    pub snapshot_pick: (Option<String>, Option<String>),
    /// Their diff, or why it could not be made.
    pub snapshot_diff: Option<Result<mcp_diff::SnapshotDiff, String>>,
    /// What the banner's diff was measured against, when it is not the last
    /// stored snapshot but a file.
    pub diff_baseline: Option<String>,
}

impl ServerEntry {
    /// The last connect failed with `e`. `unauthorized` says whether this
    /// one was turned away for want of authorization, so a later failure of
    /// another kind is not taken for a refusal; the challenge of the last
    /// refusal is kept either way for the next authorization.
    pub fn connect_failed(&mut self, e: &mcp_core::Error) {
        self.unauthorized = matches!(e, mcp_core::Error::AuthRequired { .. });
        if let mcp_core::Error::AuthRequired { challenge } = e {
            self.auth_challenge = challenge.clone();
        }
        self.status = Status::Error(explain(e, Some(&self.record.spec)));
    }

    /// What this server can do now: its feature table, from the era of the
    /// version its last session agreed on and the capabilities it declared.
    pub fn features(&self) -> Features {
        let none = Value::Null;
        let snapshot = self.snapshot();
        Features::of(
            &self.status,
            snapshot.map(|s| mcp_core::Era::of(&s.protocol_version)),
            snapshot.map_or(&none, |s| &s.capabilities),
        )
    }

    fn new(record: ServerRecord) -> Self {
        Self {
            record,
            status: Status::Off,
            session: None,
            snapshot: None,
            log: Vec::new(),
            next_log_id: 0,
            log_bytes: 0,
            last_call_ms: None,
            diff: None,
            diff_dismissed: false,
            diff_expanded: false,
            history: Vec::new(),
            history_load: HistoryLoad::Read,
            search_text: RefCell::new(HashMap::new()),
            history_waiters: Vec::new(),
            rev: 0,
            generation: 0,
            changed_items: HashSet::new(),
            subscribed: BTreeSet::new(),
            resource_updates: HashMap::new(),
            subscription_error: None,
            log_level: None,
            unauthorized: false,
            auth_challenge: None,
            roots: None,
            snapshots: None,
            snapshot_pick: (None, None),
            snapshot_diff: None,
            diff_baseline: None,
        }
    }

    /// Whether the server changed item `name` of `mode` since it was last
    /// selected.
    pub fn is_changed(&self, mode: Mode, name: &str) -> bool {
        self.changed_items.contains(&(mode, name.to_owned()))
    }

    /// Stop marking item `name` of `mode` as changed.
    fn seen(&mut self, mode: Mode, name: &str) {
        if self.changed_items.remove(&(mode, name.to_owned())) {
            self.rev += 1;
        }
    }

    /// Whether this session is subscribed to resource `uri`.
    pub fn is_subscribed(&self, uri: &str) -> bool {
        self.subscribed.contains(uri)
    }

    fn set_subscribed(&mut self, uri: String, on: bool) {
        let changed = if on {
            self.subscribed.insert(uri)
        } else {
            self.subscribed.remove(&uri)
        };
        if changed {
            self.rev += 1;
        }
    }

    /// When resource `uri` last reported a change.
    pub fn resource_updated_at(&self, uri: &str) -> Option<&str> {
        self.resource_updates.get(uri).map(String::as_str)
    }

    /// Put the lists of `kinds` from `fresh` in place of the current ones and
    /// mark the items added or altered. The other lists stay as they are now:
    /// a relist of another kind may have replaced them meanwhile.
    fn apply_relisted(&mut self, mut fresh: Snapshot, kinds: &[ListKind]) {
        let Some(current) = self.snapshot.as_mut() else {
            return;
        };
        let mut marks = Vec::new();
        for &kind in kinds {
            let mode = Mode::from(kind);
            match kind {
                ListKind::Tools => {
                    let tools = std::mem::take(&mut fresh.tools);
                    marks.extend(altered(&current.tools, &tools, |t| &t.name).map(|n| (mode, n)));
                    current.tools = tools;
                }
                ListKind::Resources => {
                    let resources = std::mem::take(&mut fresh.resources);
                    let templates = std::mem::take(&mut fresh.resource_templates);
                    marks.extend(
                        altered(&current.resources, &resources, |r| &r.uri).map(|n| (mode, n)),
                    );
                    marks.extend(
                        altered(&current.resource_templates, &templates, |t| &t.uri_template)
                            .map(|n| (mode, n)),
                    );
                    current.resources = resources;
                    current.resource_templates = templates;
                }
                ListKind::Prompts => {
                    let prompts = std::mem::take(&mut fresh.prompts);
                    marks.extend(
                        altered(&current.prompts, &prompts, |p| &p.name).map(|n| (mode, n)),
                    );
                    current.prompts = prompts;
                }
            }
            let methods = mode.list_methods();
            current
                .list_failures
                .retain(|f| !methods.contains(&f.method.as_str()));
            current.list_failures.extend(
                fresh
                    .list_failures
                    .iter()
                    .filter(|f| methods.contains(&f.method.as_str()))
                    .cloned(),
            );
        }
        current.taken_at = fresh.taken_at;
        self.changed_items.extend(marks);
        self.rev += 1;
    }

    /// Whether the calls the database holds for this server are listed yet.
    pub fn history_load(&self) -> HistoryLoad {
        self.history_load
    }

    /// Last snapshot.
    pub fn snapshot(&self) -> Option<&Snapshot> {
        self.snapshot.as_ref()
    }

    /// Replace the last snapshot.
    pub fn set_snapshot(&mut self, snapshot: Option<Snapshot>) {
        self.snapshot = snapshot;
        self.rev += 1;
    }

    /// Recorded calls, newest first.
    pub fn history(&self) -> &[CallRecord] {
        &self.history
    }

    /// The History rows, each carrying the text the filter matches. The
    /// text is built once per call and kept until the call leaves.
    fn history_items(&self) -> Vec<Item> {
        let mut texts = self.search_text.borrow_mut();
        if texts.len() > self.history.len() {
            let listed: HashSet<&str> = self.history.iter().map(|c| c.id.as_str()).collect();
            texts.retain(|id, _| listed.contains(id.as_str()));
        }
        self.history
            .iter()
            .map(|call| {
                let text = texts
                    .entry(call.id.clone())
                    .or_insert_with(|| Rc::from(search_text(call)))
                    .clone();
                history_item(call, text)
            })
            .collect()
    }

    /// Put a recorded call at the top of the history, keeping
    /// [`MAX_HISTORY`] rows. A call already listed stays listed once: the
    /// stored history may have been read after the call was written.
    pub fn push_call(&mut self, record: CallRecord) {
        if self.history.iter().any(|call| call.id == record.id) {
            return;
        }
        self.history.insert(0, record);
        self.history.truncate(MAX_HISTORY);
        self.rev += 1;
    }

    /// Replace the recorded calls, newest first.
    pub(crate) fn set_history(&mut self, calls: Vec<CallRecord>) {
        self.history = calls;
        self.rev += 1;
    }

    /// Log rows, oldest first.
    pub fn log(&self) -> &[LogRow] {
        &self.log
    }

    /// Id of the oldest row still logged, or of the next row when the log is
    /// empty: every row with a lower id has left the log. Ids are handed out
    /// in order and rows leave only from the front, so the row at index `ix`
    /// has id `first_log_id() + ix`.
    pub fn first_log_id(&self) -> u64 {
        self.log.first().map_or(self.next_log_id, |row| row.row_id)
    }

    /// Id the next row pushed gets.
    pub fn next_log_id(&self) -> u64 {
        self.next_log_id
    }

    /// Append a log row under the next id, dropping the oldest beyond the row
    /// cap or the byte budget; the row just added always stays. Returns how
    /// many rows were dropped.
    pub fn push_log(&mut self, mut row: LogRow) -> usize {
        row.row_id = self.next_log_id;
        self.next_log_id += 1;
        self.log_bytes += row.bytes;
        self.log.push(row);
        let mut excess = 0;
        for row in &self.log {
            let left = self.log.len() - excess;
            if left <= MAX_LOG_ROWS && (self.log_bytes <= LOG_BYTE_BUDGET || left == 1) {
                break;
            }
            self.log_bytes = self.log_bytes.saturating_sub(row.bytes);
            excess += 1;
        }
        self.log.drain(..excess);
        excess
    }

    /// Empty the log. Ids carry on from where it ended.
    fn clear_log(&mut self) {
        self.log.clear();
        self.log_bytes = 0;
    }

    /// Mark where a new session's rows start. The previous session's rows
    /// stay: its stderr and its failure may be the only record of why it
    /// ended. A session that logged nothing adds no second separator.
    /// Returns how many rows the cap dropped.
    fn break_log(&mut self, at: time::OffsetDateTime) -> usize {
        if self.log.last().is_some_and(|row| !row.session_break) {
            self.push_log(LogRow::session_break(at))
        } else {
            0
        }
    }

    /// The transport as a label: `Stdio` or `HTTP`.
    pub fn transport(&self) -> &'static str {
        match self.record.spec {
            ServerSpec::Stdio { .. } => "Stdio",
            ServerSpec::Http { .. } => "HTTP",
        }
    }
}

/// How far a server's stored history has been read. It is read the first
/// time its History is shown, not at startup: every server's calls carry
/// their full arguments and results.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryLoad {
    /// Not read yet. Calls recorded this session are listed already.
    Unread,
    /// Being read off the GPUI thread.
    Loading,
    /// Read, or there is nothing stored to read.
    Read,
}

/// What the detail pane shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// Selected item (or an empty state).
    Detail,
    /// The add-server form.
    AddServer,
}

/// An entry of the middle list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// Tool name, resource URI, prompt name or call id: the selection key.
    pub name: String,
    /// What the row shows (the same as `name` except in history).
    pub label: String,
    /// Right-aligned muted text (history: the time).
    pub meta: Option<String>,
    /// Whether the row is drawn in the error colour (history: failed calls).
    pub failed: bool,
    /// Whether the server added or altered the item since it was selected.
    pub changed: bool,
    /// Whether the session is subscribed to the item (resources).
    pub watched: bool,
    /// What else the filter may match, lowercased: a history row carries
    /// the call's kind, status, error, arguments and result.
    pub text: Option<Rc<str>>,
}

impl Item {
    fn named(name: &str, label: &str) -> Self {
        Self {
            name: name.to_owned(),
            label: label.to_owned(),
            meta: None,
            failed: false,
            changed: false,
            text: None,
            watched: false,
        }
    }
}

/// What the middle list was built from.
#[derive(Debug, PartialEq, Eq)]
struct ItemsKey {
    server: Option<String>,
    rev: u64,
    mode: Mode,
    filter: String,
}

/// The middle list, built once per change of what it is built from rather
/// than on every read: a render, a selection and every lookup of the selected
/// item read it.
#[derive(Debug, Default)]
struct ItemsCache {
    key: Option<ItemsKey>,
    rows: Rc<[Item]>,
    /// Rows before the filter.
    total: usize,
}

/// Emitted after every mutation.
#[derive(Debug, Clone, Copy)]
pub struct Changed;

/// Emitted when what the views keep beside the model lost its subject, so
/// they can let go of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gone {
    /// Every row of server `server_id`'s log with an id below `before` has
    /// left it (the cap, the byte budget, Clear).
    LogRows {
        /// The server whose log it was.
        server_id: String,
        /// The id of the oldest row that may still be logged.
        before: u64,
    },
    /// A server was deleted.
    Server {
        /// Its id, which every server-scoped key embeds.
        id: String,
        /// Ids of the recorded calls it listed, which history keys embed.
        calls: Vec<String>,
    },
    /// Recorded calls left a server's history: History was cleared.
    Calls {
        /// Their ids, which history keys embed.
        calls: Vec<String>,
    },
}

/// What filing a batch of session events did.
#[derive(Debug, Default, PartialEq, Eq)]
struct Applied {
    /// Whether anything a view shows changed.
    changed: bool,
    /// Set when rows left the server's log: every row with a lower id is gone.
    rows_left_before: Option<u64>,
    /// Lists the server said changed, each once.
    relist: Vec<ListKind>,
    /// Resources the server said changed, each once.
    updated: Vec<String>,
}

/// What applying a delete did.
#[derive(Debug, PartialEq, Eq)]
enum Deletion {
    /// The server was no longer listed.
    Unknown,
    /// The database refused; the server stays and the failure is shown.
    Kept,
    /// The server left, and the views may let go of what they kept for it.
    Removed(Gone),
}

/// Outcome of reading a client configuration into the sidebar.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Imported {
    /// Servers added.
    pub added: usize,
    /// Entries the file did not describe, names already in the sidebar, and
    /// servers the database turned down (a secret in the spec).
    pub skipped: usize,
    /// Servers the database failed to write, or every server when there is
    /// no database; [`AppState::persistence`] says why.
    pub not_saved: usize,
    /// Servers added without the bearer token they will need.
    pub need_token: usize,
    /// One line per problem, for the person who ran the import.
    pub notes: Vec<String>,
}

impl Imported {
    /// One line for the status bar.
    pub fn summary(&self) -> String {
        let mut parts = vec![format!(
            "imported {} server{}",
            self.added,
            if self.added == 1 { "" } else { "s" }
        )];
        if self.skipped > 0 {
            parts.push(format!("{} skipped", self.skipped));
        }
        if self.not_saved > 0 {
            parts.push(format!("{} not saved", self.not_saved));
        }
        if self.need_token > 0 {
            parts.push(format!("{} need a token", self.need_token));
        }
        parts.join(" · ")
    }
}

/// One server of an import, after its row and token were written.
#[derive(Debug)]
struct ImportedServer {
    name: String,
    /// The file named a bearer or OAuth server without a token.
    needs_token: bool,
    saved: Result<SavedServer, Unsaved>,
}

/// The application model.
pub struct AppState {
    pub(crate) bridge: Option<Bridge>,
    pub(crate) store: Option<Store>,
    /// Whether changes reach the database; the status bar shows a failure.
    pub persistence: Persistence,
    /// Where bearer tokens and OAuth credentials live (keyring in the app,
    /// memory in tests and demos).
    pub secrets: Arc<dyn mcp_auth::SecretStore>,
    /// Servers in sidebar order.
    pub servers: Vec<ServerEntry>,
    /// Selected server index.
    pub selected_server: Option<usize>,
    /// Active list.
    pub mode: Mode,
    /// Selected row of the filtered list.
    pub selected_item: Option<usize>,
    /// Filter text.
    pub filter: String,
    /// Whether the log drawer is expanded.
    pub drawer_open: bool,
    /// Whether the open drawer covers the columns, for reading the log alone.
    pub drawer_zoomed: bool,
    /// Whether the response panel shows its body under its header. Collapsed,
    /// the header stays and the input has the rest of the pane.
    pub response_open: bool,
    /// Ids of the open rows of the selected server's log. Rows open and
    /// close independently of each other.
    pub expanded_log: BTreeSet<u64>,
    /// Detail pane content.
    pub screen: Screen,
    /// Dark (true) or light theme.
    pub dark: bool,
    /// Responses per selection.
    pub responses: Responses,
    /// Which log rows are shown.
    pub log_filter: LogFilter,
    /// Lowest server log level the drawer shows; `None` shows every level.
    pub log_min_level: Option<&'static str>,
    /// Server-initiated requests waiting for an answer, oldest first.
    pub pending: Vec<PendingRequest>,
    /// The destructive action awaiting confirmation.
    pub confirm: Option<Confirm>,
    /// Server index the add/edit form is editing (`None` = adding).
    pub editing: Option<usize>,
    /// The server the form saved and is connecting. The form stays open on
    /// it, showing how the connect goes under the fields, and closes once
    /// the connection is made; a failure leaves the settings there to
    /// change and try again.
    pub form_awaits: Option<usize>,
    /// How long a quiet server goes before the session pings it.
    pub keepalive: Option<Duration>,
    /// How an OAuth authorization URL is opened: the system browser, unless
    /// a test drives the flow itself.
    pub oauth_open: Option<mcp_auth::Opener>,
    /// The filtered middle list, see [`Self::items`].
    items: RefCell<ItemsCache>,
}

/// A destructive action waiting for the person to confirm it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirm {
    /// Delete server `ix`, with its history, snapshots and stored secret.
    Delete(usize),
    /// Delete server `ix`'s recorded calls.
    ClearHistory(usize),
    /// Remove server `ix`'s stored bearer token or OAuth credentials.
    ForgetCredentials(usize),
}

impl Confirm {
    /// The index of the server the action is for.
    pub fn server(self) -> usize {
        match self {
            Self::Delete(ix) | Self::ClearHistory(ix) | Self::ForgetCredentials(ix) => ix,
        }
    }

    /// The same action for the server now at `ix`.
    fn for_server(self, ix: usize) -> Self {
        match self {
            Self::Delete(_) => Self::Delete(ix),
            Self::ClearHistory(_) => Self::ClearHistory(ix),
            Self::ForgetCredentials(_) => Self::ForgetCredentials(ix),
        }
    }
}

/// A server-initiated request the UI must answer.
#[derive(Debug, Clone)]
pub struct PendingRequest {
    /// Server the request came from.
    pub server_id: String,
    /// Its display name.
    pub server_name: String,
    /// The request and its answer slot.
    pub request: mcp_core::ServerRequest,
}

/// Log drawer filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFilter {
    /// Everything.
    All,
    /// Outbound requests and their responses.
    Requests,
    /// Notifications in either direction.
    Notifications,
    /// JSON-RPC errors and failures.
    Errors,
    /// The server process's stderr.
    Stderr,
}

impl LogFilter {
    /// All filters in header order.
    pub const ALL: [LogFilter; 5] = [
        LogFilter::All,
        LogFilter::Requests,
        LogFilter::Notifications,
        LogFilter::Errors,
        LogFilter::Stderr,
    ];

    /// Header label.
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Requests => "requests",
            Self::Notifications => "notifications",
            Self::Errors => "errors",
            Self::Stderr => "stderr",
        }
    }

    /// Whether `row` passes the filter. A session separator passes every
    /// filter, so the boundary between two connections is never hidden.
    pub fn accepts(self, row: &LogRow) -> bool {
        row.session_break
            || match self {
                Self::All => true,
                Self::Requests => row.dir != Dir::Note,
                Self::Notifications => row.dir == Dir::Note && row.method != "stderr",
                Self::Errors => row.is_error,
                Self::Stderr => row.method == "stderr",
            }
    }
}

impl EventEmitter<Changed> for AppState {}

impl EventEmitter<Gone> for AppState {}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("servers", &self.servers.len())
            .field("selected_server", &self.selected_server)
            .field("mode", &self.mode)
            .field("selected_item", &self.selected_item)
            .field("screen", &self.screen)
            .field("persistence", &self.persistence)
            .finish_non_exhaustive()
    }
}

/// Rows one server's log keeps; the oldest go first.
const MAX_LOG_ROWS: usize = 5000;
/// Payload bytes one log row keeps: a row is for reading a message's shape,
/// and a call's whole result stays in its response pane.
pub const LOG_PAYLOAD_LIMIT: usize = 64 * 1024;
/// Serialized payload bytes one server's log keeps, ~6.5 KB a row at the row
/// cap, so a session of large results cannot hold unbounded memory. It counts
/// compact JSON, not the parsed values, which take a few times as much.
pub const LOG_BYTE_BUDGET: usize = 32 * 1024 * 1024;
/// History rows kept per server (the database keeps everything).
pub const MAX_HISTORY: usize = 500;
/// Stored snapshots the Server view lists, newest first.
const MAX_SNAPSHOTS: usize = 50;
/// Settings key of the persisted theme choice.
const THEME_SETTING: &str = "theme.dark";

/// Name of the Server view's Settings section, whose row opens the edit form.
pub const SETTINGS_SECTION: &str = "settings";

/// Settings key of the roots offered to server `server_id`.
pub(crate) fn roots_setting(server_id: &str) -> String {
    format!("roots.{server_id}")
}

impl AppState {
    /// Load saved servers from `store`. Pass `None`s for a demo/test model,
    /// which keeps everything in memory on purpose.
    pub fn new(bridge: Option<Bridge>, store: Option<Store>) -> Self {
        let mut state = Self {
            bridge,
            store: store.clone(),
            persistence: if store.is_some() {
                Persistence::Saved
            } else {
                Persistence::Memory
            },
            secrets: Arc::new(mcp_auth::MemoryStore::new()),
            servers: Vec::new(),
            selected_server: None,
            mode: Mode::Tools,
            selected_item: None,
            filter: String::new(),
            drawer_open: false,
            drawer_zoomed: false,
            response_open: true,
            expanded_log: BTreeSet::new(),
            screen: Screen::Detail,
            dark: true,
            responses: Responses::default(),
            log_filter: LogFilter::All,
            log_min_level: None,
            pending: Vec::new(),
            confirm: None,
            editing: None,
            form_awaits: None,
            keepalive: SessionOptions::default().keepalive,
            oauth_open: None,
            items: RefCell::default(),
        };
        if let Some(store) = store {
            state.load(&store);
        }
        state
    }

    /// A model for an app whose database could not be opened: it runs, shows
    /// `reason` and refuses every change it could not keep.
    pub fn without_database(bridge: Option<Bridge>, reason: String) -> Self {
        let mut state = Self::new(bridge, None);
        state.persistence = Persistence::Unavailable(reason);
        state
    }

    /// Read the saved servers and the theme.
    ///
    /// Both are read here, on the thread that builds the model, because the
    /// first frame draws the sidebar in that theme; they are a few small
    /// rows. A server's recorded calls are not: they are read the first time
    /// its History is shown ([`Self::show_history`]), off the GPUI thread.
    fn load(&mut self, store: &Store) {
        let records = self
            .note_write("saved servers could not be read", store.list_servers())
            .unwrap_or_default();
        for record in records {
            let mut entry = ServerEntry::new(record);
            entry.history_load = HistoryLoad::Unread;
            self.servers.push(entry);
        }
        self.selected_server = (!self.servers.is_empty()).then_some(0);
        let dark = store.get_setting::<bool>(THEME_SETTING);
        self.dark = self
            .note_write("theme could not be read", dark)
            .flatten()
            .unwrap_or(true);
    }

    /// Log indices of the selected server's rows that pass the filter. The
    /// drawer takes them once per frame and borrows each row it draws.
    pub fn visible_log(&self) -> Vec<usize> {
        self.server()
            .map(|s| {
                s.log
                    .iter()
                    .enumerate()
                    .filter(|(_, r)| self.log_row_passes(r))
                    .map(|(i, _)| i)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Row `id` of server `server_id`'s log (see [`LogRow::row_id`]) and that
    /// server's spec, while the row is kept.
    pub fn log_row_by_id(&self, server_id: &str, id: u64) -> Option<(&LogRow, &ServerSpec)> {
        let entry = self.servers.iter().find(|s| s.record.id == server_id)?;
        let ix = entry.log.binary_search_by_key(&id, |row| row.row_id).ok()?;
        Some((&entry.log[ix], &entry.record.spec))
    }

    /// Change the log filter.
    pub fn set_log_filter(&mut self, filter: LogFilter, cx: &mut Context<Self>) {
        self.log_filter = filter;
        self.expanded_log.clear();
        self.changed(cx);
    }

    /// Hide server log messages below `level` in the drawer; `None` shows
    /// every level. Nothing is asked of the server.
    pub fn set_log_min_level(&mut self, level: Option<&'static str>, cx: &mut Context<Self>) {
        self.log_min_level = level;
        self.expanded_log.clear();
        self.changed(cx);
    }

    /// The drawer's level: hide log messages below `level`, and ask a server
    /// that accepts `logging/setLevel` to send from that level. `None` shows
    /// every level, and asks for `debug` once a level was asked for.
    pub fn choose_log_level(&mut self, level: Option<&'static str>, cx: &mut Context<Self>) {
        self.set_log_min_level(level, cx);
        if !self.features().get(Feature::LogLevel).is_available() {
            return;
        }
        let asked = self.server().and_then(|s| s.log_level.clone());
        let wanted = level.unwrap_or("debug");
        if (level.is_some() || asked.is_some()) && asked.as_deref() != Some(wanted) {
            self.set_log_level(wanted, cx);
        }
    }

    /// Whether `row` passes the drawer's kind filter and its level filter.
    pub fn log_row_passes(&self, row: &LogRow) -> bool {
        self.log_filter.accepts(row) && level_passes(self.log_min_level, row)
    }

    pub(crate) fn changed(&mut self, cx: &mut Context<Self>) {
        cx.emit(Changed);
        cx.notify();
    }

    /// Selected server, if any.
    pub fn server(&self) -> Option<&ServerEntry> {
        self.selected_server.and_then(|i| self.servers.get(i))
    }

    /// Snapshot of the selected server.
    pub fn snapshot(&self) -> Option<&Snapshot> {
        self.server().and_then(ServerEntry::snapshot)
    }

    /// The lists behind `mode` that the selected server advertised but could
    /// not deliver, so an empty list is not mistaken for an empty server.
    pub fn list_failures(&self, mode: Mode) -> Vec<ListFailure> {
        let Some(snap) = self.snapshot() else {
            return Vec::new();
        };
        snap.list_failures
            .iter()
            .filter(|f| mode.list_methods().contains(&f.method.as_str()))
            .cloned()
            .collect()
    }

    /// Item count per mode for the selected server (empty when none).
    pub fn count(&self, mode: Mode) -> Option<usize> {
        let snap = self.snapshot()?;
        Some(match mode {
            Mode::Tools => snap.tools.len(),
            Mode::Resources => snap.resources.len() + snap.resource_templates.len(),
            Mode::Prompts => snap.prompts.len(),
            // No number until the stored calls are read, so a server whose
            // history is still unread does not look as if it had none.
            Mode::History => {
                return self
                    .server()
                    .filter(|s| s.history_load == HistoryLoad::Read)
                    .map(|s| s.history.len());
            }
            Mode::Server => return None,
        })
    }

    /// Whether the History view of the selected server waits on its stored
    /// calls.
    pub fn history_reading(&self) -> bool {
        self.mode == Mode::History
            && self
                .server()
                .is_some_and(|s| s.history_load != HistoryLoad::Read)
    }

    /// The selected server when the detail pane is its settings form in
    /// place of a row's detail: off, or its connect failed, outside History,
    /// which stays readable without a session, and outside the add screen.
    /// The one rule the pane, the list, the sidebar pencil and the status bar
    /// all follow.
    pub fn settings_pane(&self) -> Option<usize> {
        if self.screen == Screen::AddServer || self.mode == Mode::History {
            return None;
        }
        let ix = self.selected_server?;
        matches!(self.servers.get(ix)?.status, Status::Off | Status::Error(_)).then_some(ix)
    }

    /// Whether a row of the middle list opens anything. History is read from
    /// the database; the other lists are what the server declared, and
    /// without a session the pane shows its settings, or that it connects,
    /// instead, so their rows are the last connection's, shown but not
    /// opened.
    pub fn list_opens(&self) -> bool {
        self.mode == Mode::History || self.server().is_some_and(|s| s.status == Status::Connected)
    }

    /// Every row of the middle list, before the filter.
    fn all_items(&self) -> Vec<Item> {
        if self.mode == Mode::History {
            return self
                .server()
                .map(ServerEntry::history_items)
                .unwrap_or_default();
        }
        if self.mode == Mode::Server {
            return if self.server().is_some() {
                server_sections(self.snapshot())
            } else {
                Vec::new()
            };
        }
        let Some(snap) = self.snapshot() else {
            return Vec::new();
        };
        let marked = |mut item: Item| {
            if let Some(server) = self.server() {
                item.changed = server.is_changed(self.mode, &item.name);
                item.watched = self.mode == Mode::Resources && server.is_subscribed(&item.name);
            }
            item
        };
        // A row shows the display title where the server gave one; the name
        // stays the key a selection is kept by.
        let titled = |name: &str, title: &Option<String>| {
            Item::named(name, title.as_deref().unwrap_or(name))
        };
        let items: Vec<Item> = match self.mode {
            Mode::Tools => snap
                .tools
                .iter()
                .map(|t| titled(&t.name, &t.title))
                .collect(),
            Mode::Resources => snap
                .resources
                .iter()
                .map(|r| titled(&r.uri, &r.title))
                .chain(
                    snap.resource_templates
                        .iter()
                        .map(|t| titled(&t.uri_template, &t.title)),
                )
                .collect(),
            Mode::Prompts => snap
                .prompts
                .iter()
                .map(|p| titled(&p.name, &p.title))
                .collect(),
            Mode::Server | Mode::History => Vec::new(),
        };
        items.into_iter().map(marked).collect()
    }

    /// Filtered rows of the middle list. Built again only when the selected
    /// server, its snapshot or history, the mode or the filter changed; every
    /// other read shares the rows built last.
    pub fn items(&self) -> Rc<[Item]> {
        let server = self.server();
        let fresh = self.items.borrow().key.as_ref().is_some_and(|key| {
            key.server.as_deref() == server.map(|s| s.record.id.as_str())
                && key.rev == server.map_or(0, |s| s.rev)
                && key.mode == self.mode
                && key.filter == self.filter
        });
        if !fresh {
            let all = self.all_items();
            let total = all.len();
            let needle = self.filter.to_lowercase();
            let rows = all
                .into_iter()
                .filter(|i| {
                    needle.is_empty()
                        || i.label.to_lowercase().contains(&needle)
                        || i.name.to_lowercase().contains(&needle)
                        || i.text.as_deref().is_some_and(|t| t.contains(&needle))
                })
                .collect();
            *self.items.borrow_mut() = ItemsCache {
                key: Some(ItemsKey {
                    server: server.map(|s| s.record.id.clone()),
                    rev: server.map_or(0, |s| s.rev),
                    mode: self.mode,
                    filter: self.filter.clone(),
                }),
                rows,
                total,
            };
        }
        self.items.borrow().rows.clone()
    }

    /// Rows of the middle list before the filter.
    pub fn items_total(&self) -> usize {
        self.items();
        self.items.borrow().total
    }

    /// `filtered/total` for the filter row, blank while the stored calls of
    /// the History on screen are read, as the sidebar count is.
    pub fn list_count(&self) -> String {
        if self.history_reading() {
            return String::new();
        }
        let shown = self.items().len();
        format!("{shown}/{}", self.items.borrow().total)
    }

    /// Name of the selected item.
    pub fn selected_name(&self) -> Option<String> {
        let items = self.items();
        self.selected_item
            .and_then(|i| items.get(i))
            .map(|i| i.name.clone())
    }

    /// Selected tool.
    pub fn selected_tool(&self) -> Option<&Tool> {
        let name = self.selected_name()?;
        self.snapshot()?.tool(&name)
    }

    /// Selected resource (concrete).
    pub fn selected_resource(&self) -> Option<&Resource> {
        let name = self.selected_name()?;
        self.snapshot()?.resource(&name)
    }

    /// Selected resource template.
    pub fn selected_template(&self) -> Option<&mcp_core::ResourceTemplate> {
        let name = self.selected_name()?;
        self.snapshot()?
            .resource_templates
            .iter()
            .find(|t| t.uri_template == name)
    }

    /// Selected prompt.
    pub fn selected_prompt(&self) -> Option<&Prompt> {
        let name = self.selected_name()?;
        self.snapshot()?.prompt(&name)
    }

    /// Selected history row.
    pub fn selected_call(&self) -> Option<&CallRecord> {
        let id = self.selected_name()?;
        self.server()?.history.iter().find(|c| c.id == id)
    }

    /// Status bar text: what the rest of the window does not say already.
    /// The counts are the sidebar's. `failure_shown` says the selected
    /// server's failure is on screen, under its settings: then it is not
    /// written twice. Only the view knows (the form may be showing its own
    /// error instead, or the zoomed log cover it), so it tells.
    pub fn status_text(&self, failure_shown: bool) -> String {
        let Some(server) = self.server() else {
            return "No server".into();
        };
        let name = &server.record.name;
        match &server.status {
            Status::Off => format!("{name} · Disconnected"),
            Status::Connecting => format!("{name} · Connecting…"),
            Status::Error(_) if failure_shown => format!("{name} · Error"),
            Status::Error(e) => format!("{name} · Error · {e}"),
            Status::Connected => {
                let mut parts = vec![name.clone(), "Connected".to_string()];
                if let Some(snap) = &server.snapshot {
                    parts.extend(
                        snap.list_failures
                            .iter()
                            .map(|f| format!("{} failed", f.method)),
                    );
                }
                if let Some(ms) = server.last_call_ms {
                    parts.push(if ms == 0 {
                        "Last call <1 ms".to_owned()
                    } else {
                        format!("Last call {ms} ms")
                    });
                }
                parts.join(" · ")
            }
        }
    }

    // ------------------------------------------------------------ selection

    /// Select a server and show its detail.
    pub fn select_server(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix < self.servers.len() {
            // Row ids are per server: another server's row may share the id.
            if self.selected_server != Some(ix) {
                self.expanded_log.clear();
            }
            self.selected_server = Some(ix);
            self.selected_item = None;
            self.filter.clear();
            self.screen = Screen::Detail;
            self.show_history(cx);
            self.show_snapshots(cx);
            self.changed(cx);
        }
    }

    /// Switch list mode.
    pub fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        if self.mode != mode {
            self.mode = mode;
            self.selected_item = None;
            self.filter.clear();
        }
        self.screen = Screen::Detail;
        self.show_history(cx);
        self.show_snapshots(cx);
        self.changed(cx);
    }

    /// Select a row of the filtered list.
    pub fn select_item(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix < self.items().len() {
            self.selected_item = Some(ix);
            self.screen = Screen::Detail;
            if let (Some(server), Some(name)) = (self.selected_server, self.selected_name()) {
                let mode = self.mode;
                self.servers[server].seen(mode, &name);
            }
            self.changed(cx);
        }
    }

    /// Select row `ix` as a click does. The Server view's Settings row opens
    /// the form that edits the server rather than a section; moving onto it
    /// with the keyboard ([`Self::select_item`]) only shows it.
    pub fn open_item(&mut self, ix: usize, cx: &mut Context<Self>) {
        if !self.list_opens() {
            return;
        }
        self.select_item(ix, cx);
        if self.mode == Mode::Server && self.selected_name().as_deref() == Some(SETTINGS_SECTION) {
            self.show_edit_selected(cx);
        }
    }

    /// Switch to `mode` and select the row named `name`. Returns whether it exists.
    pub fn select_named(&mut self, mode: Mode, name: &str, cx: &mut Context<Self>) -> bool {
        self.set_mode(mode, cx);
        let ix = self.items().iter().position(|i| i.name == name);
        match ix {
            Some(ix) => {
                self.select_item(ix, cx);
                true
            }
            None => false,
        }
    }

    /// Move the list selection by `delta`, clamped. Nothing moves while the
    /// History on screen is read, its rows not drawn yet, nor in a list whose
    /// rows open nothing ([`Self::list_opens`]).
    pub fn move_item(&mut self, delta: isize, cx: &mut Context<Self>) {
        let len = self.items().len();
        if len == 0 || self.history_reading() || !self.list_opens() {
            return;
        }
        let next = match self.selected_item {
            None if delta > 0 => 0,
            None => len - 1,
            Some(i) => (i as isize + delta).clamp(0, len as isize - 1) as usize,
        };
        self.select_item(next, cx);
    }

    /// Move the server selection by `delta`, clamped.
    pub fn move_server(&mut self, delta: isize, cx: &mut Context<Self>) {
        let len = self.servers.len();
        if len == 0 {
            return;
        }
        let next = match self.selected_server {
            None => 0,
            Some(i) => (i as isize + delta).clamp(0, len as isize - 1) as usize,
        };
        self.select_server(next, cx);
    }

    /// Update the filter text.
    pub fn set_filter(&mut self, text: String, cx: &mut Context<Self>) {
        if self.filter != text {
            self.filter = text;
            self.selected_item = None;
            self.changed(cx);
        }
    }

    /// Expand or collapse the log drawer. Collapsing drops the zoom, so the
    /// next expansion is the drawer at its height, not the whole window.
    pub fn toggle_drawer(&mut self, cx: &mut Context<Self>) {
        self.drawer_open = !self.drawer_open;
        if !self.drawer_open {
            self.drawer_zoomed = false;
        }
        self.changed(cx);
    }

    /// Show the response panel's body, or collapse it to its header.
    pub fn toggle_response(&mut self, cx: &mut Context<Self>) {
        self.response_open = !self.response_open;
        self.changed(cx);
    }

    /// Zoom the log drawer over the columns, or restore them. A collapsed
    /// drawer is expanded and zoomed in one step.
    pub fn toggle_drawer_zoom(&mut self, cx: &mut Context<Self>) {
        if self.drawer_open {
            self.drawer_zoomed = !self.drawer_zoomed;
        } else {
            self.drawer_open = true;
            self.drawer_zoomed = true;
        }
        self.changed(cx);
    }

    /// Open the log row with id `id`, or close it when open. The other open
    /// rows stay as they are: a request reads beside its response.
    pub fn toggle_log_row(&mut self, id: u64, cx: &mut Context<Self>) {
        if !self.expanded_log.remove(&id) {
            self.expanded_log.insert(id);
        }
        self.changed(cx);
    }

    /// Delete the selected call of the History on screen. One record is
    /// small and the row names it, so no confirmation, unlike clearing all.
    pub fn delete_selected_call(&mut self, cx: &mut Context<Self>) {
        if let Some(call) = self.selected_call().map(|c| c.id.clone()) {
            self.delete_call(call, cx);
        }
    }

    /// Delete one recorded call of the selected server: the database row
    /// off the GPUI thread, then the row on screen and what was kept for it.
    pub fn delete_call(&mut self, call: String, cx: &mut Context<Self>) {
        let Some(entry) = self.selected_server.and_then(|ix| self.servers.get(ix)) else {
            return;
        };
        let id = entry.record.id.clone();
        let store = self.store.clone();
        let row = call.clone();
        self.in_background(
            move || store.map(|s| s.delete_call(&row).map_err(|e| e.to_string())),
            move |state, deleted, cx| {
                let Some(pos) = state.servers.iter().position(|s| s.record.id == id) else {
                    return;
                };
                let failure = match deleted {
                    Some(Some(Err(e))) => Some(e),
                    None => Some("delete task failed".to_owned()),
                    Some(None | Some(Ok(_))) => None,
                };
                if let Some(e) = failure {
                    state.note_failure("the call was not deleted", e);
                    state.changed(cx);
                    return;
                }
                let was_selected = state.selected_server == Some(pos)
                    && state.mode == Mode::History
                    && state.selected_call().is_some_and(|c| c.id == call);
                let entry = &mut state.servers[pos];
                let kept = entry
                    .history
                    .iter()
                    .filter(|c| c.id != call)
                    .cloned()
                    .collect();
                entry.set_history(kept);
                state.responses.forget_call(&id, &call);
                if was_selected {
                    state.selected_item = None;
                }
                cx.emit(Gone::Calls { calls: vec![call] });
                state.changed(cx);
            },
            cx,
        );
    }

    /// Clear the selected server's log.
    pub fn clear_log(&mut self, cx: &mut Context<Self>) {
        if let Some(entry) = self.selected_server.and_then(|ix| self.servers.get_mut(ix)) {
            entry.clear_log();
            let gone = Gone::LogRows {
                server_id: entry.record.id.clone(),
                before: entry.first_log_id(),
            };
            self.expanded_log.clear();
            cx.emit(gone);
            self.changed(cx);
        }
    }

    /// Append a row to server `ix`'s log. When the cap or the byte budget
    /// dropped rows, returns the id every row below has left the log with.
    fn append_log(&mut self, ix: usize, row: LogRow) -> Option<u64> {
        let entry = self.servers.get_mut(ix)?;
        if entry.push_log(row) == 0 {
            return None;
        }
        let before = entry.first_log_id();
        self.log_rows_left(ix, before);
        Some(before)
    }

    /// The rows with ids below `before` left server `ix`'s log: an open row
    /// stays on its row, and is forgotten when it was one of them.
    fn log_rows_left(&mut self, ix: usize, before: u64) {
        if self.selected_server == Some(ix) {
            self.expanded_log.retain(|&id| id >= before);
        }
    }

    /// Show the add-server form.
    pub fn show_add_server(&mut self, cx: &mut Context<Self>) {
        self.editing = None;
        self.screen = Screen::AddServer;
        self.changed(cx);
    }

    /// Show the form prefilled with the selected server's settings.
    pub fn show_edit_selected(&mut self, cx: &mut Context<Self>) {
        if let Some(ix) = self.selected_server
            && ix < self.servers.len()
        {
            self.editing = Some(ix);
            self.screen = Screen::AddServer;
            self.changed(cx);
        }
    }

    /// Leave the add-server form.
    pub fn cancel_add_server(&mut self, cx: &mut Context<Self>) {
        self.editing = None;
        self.form_awaits = None;
        self.screen = Screen::Detail;
        self.changed(cx);
    }

    /// Connect server `ix`, just saved from the form (or saved already, as
    /// the form holds it), with the form kept open on it until the
    /// connection is made. Nobody is moved to a pane with nothing to show: a
    /// failure is written under the fields, where the settings can be
    /// changed and connected again.
    pub fn await_connect(&mut self, ix: usize, cx: &mut Context<Self>) {
        // Selecting shows the detail pane; the form is put back over it.
        self.select_server(ix, cx);
        // Nothing to wait for without a runtime to connect on.
        if self.bridge.is_none() {
            self.editing = None;
            return;
        }
        self.editing = Some(ix);
        self.screen = Screen::AddServer;
        self.form_awaits = Some(ix);
        self.connect(ix, cx);
    }

    /// Save server `ix` under a new name, spec and protocol, then reconnect
    /// it. `token` is a bearer token typed into the form.
    ///
    /// The database and keyring writes run off the GPUI thread
    /// ([`persistence::replace_server`]); the sidebar changes only once the
    /// row is saved, and the returned [`Saving`] says whether it was.
    pub fn update_server(
        &mut self,
        ix: usize,
        name: &str,
        spec: ServerSpec,
        protocol: ProtocolMode,
        token: Option<String>,
        cx: &mut Context<Self>,
    ) -> Saving {
        let Some(entry) = self.servers.get(ix) else {
            return refused("no such server".into());
        };
        let store = match self.writable_store() {
            Ok(store) => store,
            Err(e) => return refused(e),
        };
        let previous = entry.record.spec.clone();
        let mut record = entry.record.clone();
        record.name = name.to_owned();
        record.spec = spec;
        record.protocol = protocol;
        let secrets = self.secrets.clone();
        let (done, saving) = oneshot::channel();
        self.in_background(
            move || {
                let token = token.as_deref();
                persistence::replace_server(store.as_ref(), &*secrets, record, &previous, token)
            },
            move |state, saved, cx| {
                let saved = saved.unwrap_or_else(|| Err("save task failed".into()));
                let _ = done.send(saved.map(|saved| {
                    if let Some(ix) = state.apply_edit(saved) {
                        state.await_connect(ix, cx);
                    }
                }));
            },
            cx,
        );
        saving
    }

    /// Show an edit the database accepted. `None` when the server was
    /// deleted while it was being saved.
    fn apply_edit(&mut self, saved: SavedServer) -> Option<usize> {
        for (what, error) in saved.lost {
            self.note_failure(&what, error);
        }
        let ix = self
            .servers
            .iter()
            .position(|s| s.record.id == saved.record.id)?;
        self.servers[ix].record = saved.record;
        Some(ix)
    }

    /// Flip the theme flag (the view applies it) and remember the choice.
    pub fn set_dark(&mut self, dark: bool, cx: &mut Context<Self>) {
        self.dark = dark;
        self.save_theme(cx);
        self.changed(cx);
    }

    fn save_theme(&mut self, cx: &mut Context<Self>) {
        let Some(store) = self.store.clone() else {
            return;
        };
        let dark = self.dark;
        self.in_background(
            move || {
                store
                    .set_setting(THEME_SETTING, &dark)
                    .map_err(|e| e.to_string())
            },
            move |state, saved, cx| {
                let saved = saved.unwrap_or_else(|| Err("theme task failed".into()));
                if state.note_write("theme was not saved", saved).is_none() {
                    state.changed(cx);
                } else if state.dark != dark {
                    // A later toggle's write may have finished first; write
                    // the current choice again so the last one wins.
                    state.save_theme(cx);
                }
            },
            cx,
        );
    }

    // ------------------------------------------------------------ servers

    /// Save a new server, connected in `protocol`, then select and connect
    /// it. `token` is a bearer token typed into the form.
    ///
    /// The database and keyring writes run off the GPUI thread
    /// ([`persistence::insert_server`]); the server appears only once its
    /// row is saved, and the returned [`Saving`] says whether it was.
    pub fn add_server(
        &mut self,
        name: &str,
        spec: ServerSpec,
        protocol: ProtocolMode,
        token: Option<String>,
        cx: &mut Context<Self>,
    ) -> Saving {
        let store = match self.writable_store() {
            Ok(store) => store,
            Err(e) => return refused(e),
        };
        let name = name.to_owned();
        let secrets = self.secrets.clone();
        let (done, saving) = oneshot::channel();
        self.in_background(
            move || {
                let token = token.as_deref();
                persistence::insert_server(store.as_ref(), &*secrets, &name, spec, protocol, token)
                    .map_err(|e| e.to_string())
            },
            move |state, saved, cx| {
                let saved = saved.unwrap_or_else(|| Err("save task failed".into()));
                let _ = done.send(saved.map(|saved| {
                    let ix = state.push_saved(saved);
                    state.await_connect(ix, cx);
                }));
            },
            cx,
        );
        saving
    }

    /// Put a server the database accepted in the sidebar, without
    /// connecting. Returns its index.
    fn push_saved(&mut self, saved: SavedServer) -> usize {
        for (what, error) in saved.lost {
            self.note_failure(&what, error);
        }
        self.servers.push(ServerEntry::new(saved.record));
        self.servers.len() - 1
    }

    /// Add every server a client configuration named. The receiver resolves
    /// with the outcome once the rows and tokens are written, off the GPUI
    /// thread.
    ///
    /// Nothing is connected: an imported file can name a dozen servers, and
    /// spawning a dozen processes is not what "import" should mean. The
    /// first new server is selected so the Connect button is one click away.
    pub fn import_servers(
        &mut self,
        config: mcp_exchange::ImportedConfig,
        cx: &mut Context<Self>,
    ) -> oneshot::Receiver<Imported> {
        let mut result = Imported {
            skipped: config.skipped.len(),
            notes: config
                .skipped
                .iter()
                .map(|(name, why)| format!("`{name}` skipped: {why}"))
                .chain(config.notes.iter().cloned())
                .collect(),
            ..Imported::default()
        };
        let mut wanted = Vec::new();
        for server in config.servers {
            if self.servers.iter().any(|e| e.record.name == server.name) {
                result.skipped += 1;
                result
                    .notes
                    .push(format!("`{}` is already in the sidebar", server.name));
                continue;
            }
            // A fresh keyring entry per server, the same shape the add form
            // mints, holding the token the file carried (if it carried one).
            let mut spec = server.spec;
            if let ServerSpec::Http { auth, .. } = &mut spec
                && let mcp_core::AuthRef::Bearer { keyring_id }
                | mcp_core::AuthRef::OAuth { keyring_id } = auth
            {
                *keyring_id = mcp_auth::new_keyring_id();
            }
            wanted.push((server.name, spec, server.token));
        }
        let store = self.writable_store();
        let secrets = self.secrets.clone();
        let (done, imported) = oneshot::channel();
        self.in_background(
            move || {
                let saved = wanted.into_iter().map(|(name, spec, token)| {
                    let needs_token = token.is_none() && keyring_id(&spec).is_some();
                    let saved = match &store {
                        // A client configuration has no protocol field.
                        Ok(store) => persistence::insert_server(
                            store.as_ref(),
                            &*secrets,
                            &name,
                            spec,
                            ProtocolMode::default(),
                            token.as_deref(),
                        ),
                        Err(e) => Err(Unsaved::Failed(e.clone())),
                    };
                    ImportedServer {
                        name,
                        needs_token,
                        saved,
                    }
                });
                saved.collect::<Vec<_>>()
            },
            move |state, saved, cx| {
                let first = match saved {
                    Some(saved) => state.apply_import(&mut result, saved),
                    None => {
                        result.notes.push("import task failed".into());
                        state.note_failure("servers were not imported", "import task failed");
                        None
                    }
                };
                match first {
                    Some(ix) => state.select_server(ix, cx),
                    None => state.changed(cx),
                }
                let _ = done.send(result);
            },
            cx,
        );
        imported
    }

    /// Put the servers an import saved in the sidebar and count every one
    /// into `result`. Returns the index of the first server added.
    fn apply_import(&mut self, result: &mut Imported, saved: Vec<ImportedServer>) -> Option<usize> {
        let mut first = None;
        for server in saved {
            match server.saved {
                Ok(saved) => {
                    result.added += 1;
                    // A token the keyring refused is one the server still needs.
                    if server.needs_token || !saved.lost.is_empty() {
                        result.need_token += 1;
                    }
                    let ix = self.push_saved(saved);
                    first.get_or_insert(ix);
                }
                Err(Unsaved::Refused(e)) => {
                    result.skipped += 1;
                    result.notes.push(format!("`{}`: {e}", server.name));
                }
                // The name was not in the sidebar, so a failure here is a
                // lost write, not an entry the import chose to leave out.
                Err(Unsaved::Failed(e)) => {
                    result.not_saved += 1;
                    let what = format!("`{}` was not imported", server.name);
                    result.notes.push(format!("{what}: {e}"));
                    self.note_failure(&what, e);
                }
            }
        }
        first
    }

    /// Insert a server without persistence or connection (demo data, tests).
    /// Returns its index.
    pub fn add_demo_server(&mut self, name: &str, spec: ServerSpec, status: Status) -> usize {
        let mut entry = ServerEntry::new(demo_record(name, spec));
        entry.status = status;
        self.servers.push(entry);
        let ix = self.servers.len() - 1;
        if self.selected_server.is_none() {
            self.selected_server = Some(ix);
        }
        ix
    }

    /// Ask before deleting the selected server (the dialog calls
    /// [`Self::confirm`] or [`Self::cancel_confirm`]).
    pub fn request_delete_selected(&mut self, cx: &mut Context<Self>) {
        self.request_confirm(Confirm::Delete, cx);
    }

    /// Ask before deleting the selected server's recorded calls.
    pub fn request_clear_history(&mut self, cx: &mut Context<Self>) {
        self.request_confirm(Confirm::ClearHistory, cx);
    }

    /// Ask before removing the selected server's stored credentials; only a
    /// server with a bearer token or OAuth has any.
    pub fn request_forget_credentials(&mut self, cx: &mut Context<Self>) {
        if self
            .server()
            .is_some_and(|s| keyring_id(&s.record.spec).is_some())
        {
            self.request_confirm(Confirm::ForgetCredentials, cx);
        }
    }

    fn request_confirm(&mut self, action: fn(usize) -> Confirm, cx: &mut Context<Self>) {
        if let Some(ix) = self.selected_server
            && ix < self.servers.len()
        {
            self.confirm = Some(action(ix));
            self.changed(cx);
        }
    }

    /// Close the confirmation without doing anything.
    pub fn cancel_confirm(&mut self, cx: &mut Context<Self>) {
        if self.confirm.take().is_some() {
            self.changed(cx);
        }
    }

    /// Do what the confirmation was opened for.
    pub fn confirm(&mut self, cx: &mut Context<Self>) {
        match self.confirm.take() {
            Some(Confirm::Delete(ix)) => self.delete_server(ix, cx),
            Some(Confirm::ClearHistory(ix)) => self.clear_history(ix, cx),
            Some(Confirm::ForgetCredentials(ix)) => self.forget_credentials(ix, cx),
            None => {}
        }
    }

    /// Delete server `ix`'s recorded calls: the rows first, off the GPUI
    /// thread, and the History list once they are gone, so a clear the
    /// database refused does not look done.
    pub fn clear_history(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(entry) = self.servers.get(ix) else {
            return;
        };
        let id = entry.record.id.clone();
        let store = self.store.clone();
        let row = id.clone();
        self.in_background(
            move || store.map(|s| s.clear_calls(Some(&row)).map_err(|e| e.to_string())),
            move |state, cleared, cx| {
                let Some(pos) = state.servers.iter().position(|s| s.record.id == id) else {
                    return;
                };
                let failure = match cleared {
                    Some(Some(Err(e))) => Some(e),
                    None => Some("clear task failed".to_owned()),
                    Some(None | Some(Ok(_))) => None,
                };
                if let Some(e) = failure {
                    let what = format!(
                        "history of `{}` was not cleared",
                        state.servers[pos].record.name
                    );
                    state.note_failure(&what, e);
                    state.changed(cx);
                    return;
                }
                let entry = &mut state.servers[pos];
                let calls = entry.history.iter().map(|call| call.id.clone()).collect();
                entry.set_history(Vec::new());
                entry.history_load = HistoryLoad::Read;
                for waiter in std::mem::take(&mut entry.history_waiters) {
                    let _ = waiter.send(());
                }
                state.responses.forget_mode(&id, Mode::History);
                if state.selected_server == Some(pos) && state.mode == Mode::History {
                    state.selected_item = None;
                }
                cx.emit(Gone::Calls { calls });
                state.changed(cx);
            },
            cx,
        );
    }

    /// Remove server `ix`'s stored bearer token or OAuth credentials from the
    /// keyring. The server stays and asks for credentials on its next
    /// connect; a session already open is not affected.
    pub fn forget_credentials(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(entry) = self.servers.get(ix) else {
            return;
        };
        let Some(key) = keyring_id(&entry.record.spec) else {
            return;
        };
        let what = format!("credentials of `{}` were not removed", entry.record.name);
        let secrets = self.secrets.clone();
        self.in_background(
            move || mcp_auth::forget(&*secrets, &key).map_err(|e| e.to_string()),
            move |state, forgot, cx| {
                match forgot {
                    Some(Ok(())) => {}
                    Some(Err(e)) => state.note_failure(&what, e),
                    None => state.note_failure(&what, "keyring task failed"),
                }
                state.changed(cx);
            },
            cx,
        );
    }

    /// Start reading the selected server's stored snapshots when its Server
    /// view is on screen and they have not been read.
    pub(crate) fn show_snapshots(&mut self, cx: &mut Context<Self>) {
        if self.mode == Mode::Server {
            self.read_snapshots(cx);
        }
    }

    /// Read the selected server's stored snapshots off the GPUI thread, once.
    /// The two newest start picked, so their diff shows without a click.
    fn read_snapshots(&mut self, cx: &mut Context<Self>) {
        let store = self.store.clone();
        let Some(entry) = self.selected_server.and_then(|ix| self.servers.get_mut(ix)) else {
            return;
        };
        if entry.snapshots.is_some() {
            return;
        }
        entry.snapshots = Some(Vec::new());
        let Some(store) = store else {
            return;
        };
        let id = entry.record.id.clone();
        let row = id.clone();
        self.in_background(
            move || {
                store
                    .list_snapshots(&row, MAX_SNAPSHOTS)
                    .map_err(|e| e.to_string())
            },
            move |state, listed, cx| {
                let Some(pos) = state.servers.iter().position(|s| s.record.id == id) else {
                    return;
                };
                match listed.unwrap_or_else(|| Err("snapshot task failed".into())) {
                    Ok(listed) => {
                        let entry = &mut state.servers[pos];
                        entry.snapshot_pick = (
                            listed.get(1).map(|s| s.id.clone()),
                            listed.first().map(|s| s.id.clone()),
                        );
                        entry.snapshots = Some(listed);
                        state.diff_snapshots(pos, cx);
                    }
                    Err(e) => {
                        let what = format!(
                            "snapshots of `{}` could not be read",
                            state.servers[pos].record.name
                        );
                        state.note_failure(&what, e);
                    }
                }
                state.changed(cx);
            },
            cx,
        );
    }

    /// Pick stored snapshot `id` as the newer side of the Server view's
    /// comparison, or the older one, and compare again.
    pub fn pick_snapshot(&mut self, id: String, newer: bool, cx: &mut Context<Self>) {
        let Some(ix) = self.selected_server else {
            return;
        };
        let Some(entry) = self.servers.get_mut(ix) else {
            return;
        };
        if newer {
            entry.snapshot_pick.1 = Some(id);
        } else {
            entry.snapshot_pick.0 = Some(id);
        }
        self.diff_snapshots(ix, cx);
        self.changed(cx);
    }

    /// Diff server `ix`'s two picked snapshots, read off the GPUI thread.
    fn diff_snapshots(&mut self, ix: usize, cx: &mut Context<Self>) {
        let store = self.store.clone();
        let Some(entry) = self.servers.get_mut(ix) else {
            return;
        };
        entry.snapshot_diff = None;
        let (Some(older), Some(newer), Some(store)) = (
            entry.snapshot_pick.0.clone(),
            entry.snapshot_pick.1.clone(),
            store,
        ) else {
            return;
        };
        let id = entry.record.id.clone();
        let pick = entry.snapshot_pick.clone();
        self.in_background(
            move || -> Result<mcp_diff::SnapshotDiff, String> {
                let read = |id: &str| -> Result<mcp_store::SnapshotRecord, String> {
                    store
                        .get_snapshot(id)
                        .map_err(|e| e.to_string())?
                        .ok_or_else(|| format!("snapshot {id} is no longer stored"))
                };
                let (older, newer) = (read(&older)?, read(&newer)?);
                Ok(mcp_diff::diff(Some(&older.snapshot), &newer.snapshot))
            },
            move |state, diffed, cx| {
                let Some(entry) = state.servers.iter_mut().find(|s| s.record.id == id) else {
                    return;
                };
                // Picked again meanwhile: that comparison is the one to show.
                if entry.snapshot_pick != pick {
                    return;
                }
                entry.snapshot_diff =
                    Some(diffed.unwrap_or_else(|| Err("diff task failed".into())));
                state.changed(cx);
            },
            cx,
        );
    }

    /// Show how the selected server's snapshot differs from `baseline`, read
    /// from the file `name`, in the change banner.
    pub fn compare_with_snapshot(
        &mut self,
        baseline: &Snapshot,
        name: String,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self.selected_server.and_then(|ix| self.servers.get_mut(ix)) else {
            return;
        };
        let Some(diff) = entry
            .snapshot()
            .map(|current| mcp_diff::diff(Some(baseline), current))
        else {
            return;
        };
        entry.diff = Some(diff);
        entry.diff_baseline = Some(name);
        entry.diff_dismissed = false;
        entry.diff_expanded = true;
        self.changed(cx);
    }

    /// Remove a server, its history and its stored secret.
    ///
    /// The database row goes first, off the GPUI thread; the server leaves
    /// the sidebar only once it is gone, so a failed delete cannot come back
    /// on the next launch. The secret is removed only after the row, so a
    /// server that stays can still connect.
    pub fn delete_server(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(entry) = self.servers.get(ix) else {
            return;
        };
        let id = entry.record.id.clone();
        let key = keyring_id(&entry.record.spec);
        let store = self.store.clone();
        let secrets = self.secrets.clone();
        let row = id.clone();
        self.in_background(
            move || {
                let stored = store.map(|s| {
                    // The roots offered to it are a setting, not a row of its own.
                    let _ = s.delete_setting(&roots_setting(&row));
                    s.delete_server(&row).map_err(|e| e.to_string())
                });
                let keyring = match (&stored, key) {
                    (None | Some(Ok(_)), Some(key)) => Some(secrets.delete(&key)),
                    _ => None,
                };
                (stored, keyring)
            },
            move |state, deleted, cx| {
                let (stored, keyring) =
                    deleted.unwrap_or_else(|| (Some(Err("delete task failed".into())), None));
                match state.finish_delete(&id, stored, keyring) {
                    Deletion::Unknown => {}
                    Deletion::Kept => state.changed(cx),
                    Deletion::Removed(gone) => {
                        cx.emit(gone);
                        // The selection may have moved onto an unread server.
                        state.show_history(cx);
                        state.changed(cx);
                    }
                }
            },
            cx,
        );
    }

    /// Apply the outcome of a delete to the model. `stored` is `None`
    /// without a database, `keyring` when there was no secret to remove.
    fn finish_delete(
        &mut self,
        id: &str,
        stored: Option<Result<bool, String>>,
        keyring: Option<Result<(), String>>,
    ) -> Deletion {
        let Some(ix) = self.servers.iter().position(|s| s.record.id == id) else {
            return Deletion::Unknown;
        };
        let name = self.servers[ix].record.name.clone();
        if let Some(Err(e)) = stored {
            self.note_failure(&format!("`{name}` was not deleted"), e);
            return Deletion::Kept;
        }
        let entry = self.servers.remove(ix);
        if let Some(session) = entry.session {
            session.close();
        }
        // Nothing the server owned outlives it: its responses, and the
        // requests it was waiting on, which dropping cancels.
        self.responses.forget_server(id);
        self.pending.retain(|p| p.server_id != id);
        // The delete ran in the background: indices taken before it may now
        // point one row too far, or at the server that is gone.
        self.selected_server = match self.selected_server {
            Some(s) if s == ix => {
                self.selected_item = None;
                self.expanded_log.clear();
                (!self.servers.is_empty()).then(|| ix.min(self.servers.len() - 1))
            }
            Some(s) if s > ix => Some(s - 1),
            other => other,
        };
        self.confirm = match self.confirm {
            Some(c) if c.server() == ix => None,
            Some(c) if c.server() > ix => Some(c.for_server(c.server() - 1)),
            other => other,
        };
        // The form keeps its own copy of the index it edits: close it rather
        // than let it save over another server.
        if self.editing.is_some_and(|e| e >= ix) {
            self.editing = None;
            if self.screen == Screen::AddServer {
                self.screen = Screen::Detail;
            }
        }
        if let Some(Err(e)) = keyring {
            let what = format!("stored token of `{name}` was not removed from the keyring");
            self.note_failure(&what, e);
        }
        Deletion::Removed(Gone::Server {
            id: id.to_owned(),
            calls: entry.history.iter().map(|call| call.id.clone()).collect(),
        })
    }

    /// Connect (or reconnect) server `ix`. Events stream into its log.
    pub fn connect(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(bridge) = self.bridge.clone() else {
            return;
        };
        let oauth = self.oauth_options(ix);
        let Some(entry) = self.servers.get_mut(ix) else {
            return;
        };
        if let Some(old) = entry.session.take() {
            old.close();
        }
        entry.generation += 1;
        let generation = entry.generation;
        entry.status = Status::Connecting;
        // The last connection's lists stay, shown as its, until this one
        // succeeds and replaces them; a connect that fails leaves them.
        // A new session has no subscriptions and nothing changed yet.
        entry.changed_items.clear();
        entry.subscribed.clear();
        entry.resource_updates.clear();
        entry.subscription_error = None;
        // It may store a snapshot of its own, and the banner is measured
        // against the stored one again.
        entry.snapshots = None;
        entry.snapshot_pick = (None, None);
        entry.snapshot_diff = None;
        entry.diff_baseline = None;
        entry.last_call_ms = None;
        entry.diff = None;
        entry.diff_dismissed = false;
        entry.diff_expanded = false;
        let id = entry.record.id.clone();
        let spec = entry.record.spec.clone();
        let policy = entry.record.policy.clone();
        let protocol = entry.record.protocol;
        let keepalive = self.keepalive;
        if let Some(gone) = self.open_session_log(ix) {
            cx.emit(gone);
        }

        let sink = EventSink::new(4096);
        // Rows are built, and large payloads cut, on the runtime.
        let mut events = bridge.forward_events(&sink, incoming);
        let event_id = id.clone();
        cx.spawn(async move |this, cx| {
            // Everything already queued when the task wakes is filed in one
            // update, so a burst from a chatty server redraws once, not once
            // per message.
            while let Some(first) = events.next().await {
                let batch = drain_ready(first, &mut events);
                let alive = this.update(cx, |state, cx| {
                    state.push_events(&event_id, generation, batch, cx);
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();

        let secrets = self.secrets.clone();
        let store = self.store.clone();
        let snapshot_owner = id.clone();
        let connect = bridge.run(async move {
            let transport = match &spec {
                ServerSpec::Http { url, auth, .. } => mcp_auth::resolve(auth, url, secrets, &oauth)
                    .await
                    .map_err(|e| mcp_core::Error::Transport(e.to_string()))?,
                ServerSpec::Stdio { .. } => Default::default(),
            };
            let options = SessionOptions {
                policy,
                protocol,
                sink: Some(sink),
                transport,
                keepalive,
                ..SessionOptions::default()
            };
            let session = Session::connect(spec, options).await?;
            let snapshot = session.snapshot().await?;
            // Comparing with the stored snapshot is blocking database work;
            // it finishes before the server shows as connected.
            // So is reading the roots offered to the server last time.
            let stored = match store {
                Some(store) => {
                    let taken = snapshot.clone();
                    let compared = tokio::task::spawn_blocking(move || {
                        let compared = compare_and_store(&store, &snapshot_owner, &taken);
                        let roots = store
                            .get_setting::<Vec<Root>>(&roots_setting(&snapshot_owner))
                            .map_err(|e| e.to_string());
                        (compared, roots)
                    })
                    .await;
                    Some(compared.unwrap_or_else(|e| {
                        let failed = format!("connect task: {e}");
                        ((None, Some(failed.clone())), Err(failed))
                    }))
                }
                None => None,
            };
            Ok::<_, mcp_core::Error>((session, snapshot, stored))
        });
        cx.spawn(async move |this, cx| {
            let result = connect.await;
            let _ = this.update(cx, |state, cx| {
                let Some(pos) = state.servers.iter().position(|s| s.record.id == id) else {
                    return;
                };
                // A write that failed is reported even when this connection
                // has been replaced in the meantime.
                let not_stored = match &result {
                    Some(Ok((_, _, Some(((_, Some(e)), _))))) => Some(e.clone()),
                    _ => None,
                };
                if let Some(e) = &not_stored {
                    let what = format!(
                        "snapshot of `{}` was not stored",
                        state.servers[pos].record.name
                    );
                    state.note_failure(&what, e);
                }
                let entry = &mut state.servers[pos];
                if entry.generation != generation {
                    if not_stored.is_some() {
                        state.changed(cx);
                    }
                    return;
                }
                let mut after = None;
                match result {
                    Some(Ok((session, snapshot, stored))) => {
                        let (compared, roots) = match stored {
                            Some((compared, roots)) => (Some(compared), Some(roots)),
                            None => (None, None),
                        };
                        entry.diff = compared.and_then(|(diff, _)| diff);
                        entry.session = Some(session);
                        entry.set_snapshot(Some(snapshot));
                        entry.status = Status::Connected;
                        entry.unauthorized = false;
                        entry.auth_challenge = None;
                        let unread = match roots {
                            Some(Ok(roots)) => {
                                entry.roots = Some(roots.unwrap_or_default());
                                None
                            }
                            Some(Err(e)) => Some(e),
                            None => None,
                        };
                        after = Some((entry.log_level.clone(), unread, entry.record.name.clone()));
                    }
                    Some(Err(e)) => entry.connect_failed(&e),
                    None => entry.status = Status::Error("connect task failed".into()),
                }
                // Connected, the form that saved the server has done its
                // job; failed, it stays with the failure under its fields.
                if state.form_awaits == Some(pos) && state.servers[pos].status == Status::Connected
                {
                    state.form_awaits = None;
                    if state.screen == Screen::AddServer && state.editing == Some(pos) {
                        state.editing = None;
                        state.screen = Screen::Detail;
                    }
                }
                if let Some((level, unread, name)) = after {
                    if state.selected_server == Some(pos) {
                        state.show_snapshots(cx);
                    }
                    if let Some(e) = unread {
                        state.note_failure(&format!("roots of `{name}` could not be read"), e);
                    }
                    // A new session starts at the server's own level.
                    if let Some(level) = level {
                        state.send_log_level(pos, level, cx);
                    }
                }
                state.changed(cx);
            });
        })
        .detach();
        self.changed(cx);
    }

    /// Log in again to server `ix`, whose OAuth credentials it refused: the
    /// stored ones are dropped and the browser flow runs again, starting
    /// from the challenge the server sent. A server without OAuth has
    /// nothing to log in to; its credentials are set in its settings.
    pub fn authorize(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(entry) = self.servers.get(ix) else {
            return;
        };
        let ServerSpec::Http {
            auth: mcp_core::AuthRef::OAuth { keyring_id },
            ..
        } = &entry.record.spec
        else {
            return;
        };
        let secrets = self.secrets.clone();
        let key = keyring_id.clone();
        let id = entry.record.id.clone();
        self.in_background(
            // Nothing may be stored yet, which is not a failure.
            move || {
                let _ = mcp_auth::forget(&*secrets, &key);
            },
            move |state, _, cx| {
                if let Some(ix) = state.servers.iter().position(|s| s.record.id == id) {
                    state.connect(ix, cx);
                }
            },
            cx,
        );
    }

    /// The OAuth options a connect of server `ix` authorizes with. The
    /// server named where its authorization metadata is when it turned the
    /// last attempt away, so the flow starts from that challenge rather than
    /// from guessed well-known locations.
    pub(crate) fn oauth_options(&self, ix: usize) -> mcp_auth::OAuthOptions {
        let open = self
            .oauth_open
            .clone()
            .unwrap_or_else(mcp_auth::browser_opener);
        mcp_auth::OAuthOptions {
            challenge: self.servers.get(ix).and_then(|s| s.auth_challenge.clone()),
            open,
            ..mcp_auth::OAuthOptions::default()
        }
    }

    /// Mark where server `ix`'s new session starts in its log. When the
    /// separator pushed old rows out, says which, for the views to let go of
    /// what they kept for them.
    fn open_session_log(&mut self, ix: usize) -> Option<Gone> {
        let entry = self.servers.get_mut(ix)?;
        if entry.break_log(time::OffsetDateTime::now_utc()) == 0 {
            return None;
        }
        let before = entry.first_log_id();
        let server_id = entry.record.id.clone();
        self.log_rows_left(ix, before);
        Some(Gone::LogRows { server_id, before })
    }

    /// Close the session of server `ix`.
    pub fn disconnect(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(entry) = self.servers.get_mut(ix) {
            entry.generation += 1;
            if let Some(session) = entry.session.take() {
                session.close();
            }
            entry.status = Status::Off;
            self.changed(cx);
        }
    }

    /// File a batch of server `id`'s events from connection `generation`, in
    /// the order they happened, with one redraw for all of them.
    fn push_events(
        &mut self,
        id: &str,
        generation: u64,
        batch: Vec<Incoming>,
        cx: &mut Context<Self>,
    ) {
        let applied = self.apply_events(id, generation, batch);
        if let Some(before) = applied.rows_left_before {
            cx.emit(Gone::LogRows {
                server_id: id.to_owned(),
                before,
            });
        }
        if !applied.relist.is_empty() {
            self.relist(id, applied.relist, cx);
        }
        for uri in &applied.updated {
            self.reread_resource(id, uri, cx);
        }
        if applied.changed {
            self.changed(cx);
        }
    }

    /// Read the lists of `kinds` of server `id` again, which the server said
    /// changed, and put them in place of the old ones without reconnecting.
    /// What was added or altered is marked in the list until selected.
    fn relist(&mut self, id: &str, kinds: Vec<ListKind>, cx: &mut Context<Self>) {
        let Some(bridge) = self.bridge.clone() else {
            return;
        };
        let Some(entry) = self.servers.iter().find(|s| s.record.id == id) else {
            return;
        };
        let (Some(session), Some(snapshot)) = (entry.session.clone(), entry.snapshot().cloned())
        else {
            return;
        };
        let generation = entry.generation;
        let run = bridge.run(async move {
            let mut fresh = snapshot;
            for &kind in &kinds {
                fresh = session.relist(&fresh, kind).await?;
            }
            Ok::<_, mcp_core::Error>((fresh, kinds))
        });
        let id = id.to_owned();
        cx.spawn(async move |this, cx| {
            let result = run.await;
            let _ = this.update(cx, |state, cx| {
                let Some(pos) = state.servers.iter().position(|s| s.record.id == id) else {
                    return;
                };
                if state.servers[pos].generation != generation {
                    return;
                }
                let failure = match result {
                    Some(Ok((fresh, kinds))) => {
                        state.servers[pos].apply_relisted(fresh, &kinds);
                        None
                    }
                    Some(Err(e)) => Some(explain(&e, Some(&state.servers[pos].record.spec))),
                    None => Some("relist task failed".to_owned()),
                };
                if let Some(e) = failure {
                    let text = format!("the changed list could not be read again: {e}");
                    let row = LogRow::new(
                        time::OffsetDateTime::now_utc(),
                        Dir::Note,
                        "relist".into(),
                        Value::String(text),
                        true,
                    );
                    if let Some(before) = state.append_log(pos, row) {
                        cx.emit(Gone::LogRows {
                            server_id: id.clone(),
                            before,
                        });
                    }
                }
                state.changed(cx);
            });
        })
        .detach();
    }

    /// Subscribe the selected server's session to changes of resource `uri`,
    /// or end the subscription it has. A subscribed resource is marked in
    /// the list, and a change the server reports reads it again.
    pub fn toggle_subscription(&mut self, uri: String, cx: &mut Context<Self>) {
        let Some(bridge) = self.bridge.clone() else {
            return;
        };
        let Some(entry) = self.server() else {
            return;
        };
        let Some(session) = entry.session.clone() else {
            return;
        };
        let id = entry.record.id.clone();
        let generation = entry.generation;
        let on = !entry.is_subscribed(&uri);
        let target = uri.clone();
        let run = bridge.run(async move {
            if on {
                session.subscribe_resource(&target).await
            } else {
                session.unsubscribe_resource(&target).await
            }
        });
        cx.spawn(async move |this, cx| {
            let result = run.await;
            let _ = this.update(cx, |state, cx| {
                let Some(entry) = state
                    .servers
                    .iter_mut()
                    .find(|s| s.record.id == id && s.generation == generation)
                else {
                    return;
                };
                match result {
                    Some(Ok(())) => {
                        entry.subscription_error = None;
                        entry.set_subscribed(uri, on);
                    }
                    Some(Err(e)) => {
                        entry.subscription_error =
                            Some((uri, explain(&e, Some(&entry.record.spec))));
                    }
                    None => {
                        entry.subscription_error = Some((uri, "subscribe task failed".into()));
                    }
                }
                state.changed(cx);
            });
        })
        .detach();
    }

    /// The feature table of the selected server; see [`Features`]. With no
    /// server selected, nothing is available.
    pub fn features(&self) -> Features {
        match self.server() {
            Some(server) => server.features(),
            None => Features::of(&Status::Off, None, &Value::Null),
        }
    }

    /// Disconnect server `ix` when it is connected, connect it when it is off
    /// or failed. A connect still on its way is left alone.
    pub fn toggle_connection(&mut self, ix: usize, cx: &mut Context<Self>) {
        match self.servers.get(ix).map(|s| &s.status) {
            Some(Status::Connected) => self.disconnect(ix, cx),
            Some(Status::Off | Status::Error(_)) => self.connect(ix, cx),
            Some(Status::Connecting) | None => {}
        }
    }

    /// Ask the selected server to send log messages at `level` and above.
    /// The choice is kept, and sent again when the server reconnects.
    pub fn set_log_level(&mut self, level: &str, cx: &mut Context<Self>) {
        if let Some(ix) = self.selected_server {
            self.send_log_level(ix, level.to_owned(), cx);
        }
    }

    fn send_log_level(&mut self, ix: usize, level: String, cx: &mut Context<Self>) {
        let Some(bridge) = self.bridge.clone() else {
            return;
        };
        let Some(entry) = self.servers.get(ix) else {
            return;
        };
        let Some(session) = entry.session.clone() else {
            return;
        };
        let id = entry.record.id.clone();
        let generation = entry.generation;
        let sent = level.clone();
        let run = bridge.run(async move { session.set_log_level(&sent).await });
        cx.spawn(async move |this, cx| {
            let result = run.await;
            let _ = this.update(cx, |state, cx| {
                let Some(pos) = state
                    .servers
                    .iter()
                    .position(|s| s.record.id == id && s.generation == generation)
                else {
                    return;
                };
                let failure = match result {
                    Some(Ok(())) => {
                        state.servers[pos].log_level = Some(level.clone());
                        None
                    }
                    Some(Err(e)) => Some(e.to_string()),
                    None => Some("log level task failed".to_owned()),
                };
                if let Some(e) = failure {
                    let row = LogRow::new(
                        time::OffsetDateTime::now_utc(),
                        Dir::Note,
                        "logging/setLevel".into(),
                        Value::String(format!("log level `{level}` was not set: {e}")),
                        true,
                    );
                    if let Some(before) = state.append_log(pos, row) {
                        cx.emit(Gone::LogRows {
                            server_id: id.clone(),
                            before,
                        });
                    }
                }
                state.changed(cx);
            });
        })
        .detach();
    }

    /// Remember `roots` as the ones offered to server `server_id` and store
    /// them. With `notify`, a connected server is told they changed, so it
    /// asks again; an answer to its own request needs no such notice.
    pub fn keep_roots(
        &mut self,
        server_id: &str,
        roots: Vec<Root>,
        notify: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self.servers.iter_mut().find(|s| s.record.id == server_id) else {
            return;
        };
        if entry.roots.as_ref() == Some(&roots) {
            return;
        }
        entry.roots = Some(roots.clone());
        let name = entry.record.name.clone();
        // A 2026-07-28 server has no change notice; it gets the roots when it
        // asks during a request.
        if notify
            && entry.features().get(Feature::RootsChanged).is_available()
            && let (Some(session), Some(bridge)) = (entry.session.clone(), self.bridge.clone())
        {
            let told = bridge.run(async move { session.notify_roots_changed().await });
            cx.spawn(async move |_, _| {
                let _ = told.await;
            })
            .detach();
        }
        if let Some(store) = self.store.clone() {
            let key = roots_setting(server_id);
            self.in_background(
                move || store.set_setting(&key, &roots).map_err(|e| e.to_string()),
                move |state, saved, cx| {
                    let saved = saved.unwrap_or_else(|| Err("roots task failed".into()));
                    if state
                        .note_write(&format!("roots of `{name}` were not saved"), saved)
                        .is_none()
                    {
                        state.changed(cx);
                    }
                },
                cx,
            );
        }
        self.changed(cx);
    }

    /// The model half of [`Self::push_events`]. The events of a connection
    /// that was replaced, or of a server that was deleted, change nothing.
    fn apply_events(&mut self, id: &str, generation: u64, batch: Vec<Incoming>) -> Applied {
        let mut applied = Applied::default();
        let Some(pos) = self.servers.iter().position(|s| s.record.id == id) else {
            return applied;
        };
        if self.servers[pos].generation != generation {
            return applied;
        }
        for event in batch {
            if let Some(kind) = event.list_changed
                && !applied.relist.contains(&kind)
            {
                applied.relist.push(kind);
            }
            if let Some(uri) = event.resource_updated {
                let at = time_label(time::OffsetDateTime::now_utc());
                self.servers[pos].resource_updates.insert(uri.clone(), at);
                if !applied.updated.contains(&uri) {
                    applied.updated.push(uri);
                }
                applied.changed = true;
            }
            if let Some(progress) = event.progress
                && self.responses.progress(progress)
            {
                applied.changed = true;
            }
            // The server gave up on a request of its own: nobody is asked
            // for an answer it no longer waits for.
            if let Some(withdrawn) = event.withdrawn {
                let before = self.pending.len();
                self.pending
                    .retain(|p| !(p.server_id == id && p.request.id == withdrawn));
                applied.changed |= self.pending.len() != before;
            }
            let entry = &mut self.servers[pos];
            if let Some((state, detail)) = event.change {
                match state {
                    // A disconnect of our own bumps the generation first, so
                    // one that arrives here is the server's doing: a failure,
                    // said as one, not the calm of a server switched off.
                    ConnectionState::Disconnected if entry.status == Status::Connected => {
                        entry.status = Status::Error("The server ended the session.".into());
                        entry.session = None;
                    }
                    // A connect that fails says why through its own result,
                    // in the pane's words; its state change, which comes
                    // on another channel and in no set order with it, must
                    // not overwrite that with the transport's text. Only a
                    // session that was up fails here.
                    ConnectionState::Failed if entry.status == Status::Connected => {
                        entry.status =
                            Status::Error(crate::explain::detail(&detail.unwrap_or_default()));
                        entry.session = None;
                    }
                    _ => {}
                }
                applied.changed = true;
            }
            if let Some(row) = event.row {
                if let Some(before) = self.append_log(pos, row) {
                    applied.rows_left_before = Some(before);
                }
                applied.changed = true;
            }
            if let Some(request) = event.request {
                self.pending.push(PendingRequest {
                    server_id: id.to_owned(),
                    server_name: self.servers[pos].record.name.clone(),
                    request,
                });
                applied.changed = true;
            }
        }
        applied
    }

    // ------------------------------------------------------------ history and diff

    /// Start reading the selected server's stored calls when its History is
    /// on screen and they have not been read.
    pub(crate) fn show_history(&mut self, cx: &mut Context<Self>) {
        if self.mode == Mode::History
            && let Some(ix) = self.selected_server
        {
            self.read_history(ix, cx);
        }
    }

    /// Resolves once server `ix`'s stored calls are listed, reading them when
    /// no one has yet. Dropped unresolved when the server is deleted first.
    pub fn history_ready(&mut self, ix: usize, cx: &mut Context<Self>) -> oneshot::Receiver<()> {
        let (done, ready) = oneshot::channel();
        if let Some(entry) = self.servers.get_mut(ix) {
            if entry.history_load == HistoryLoad::Read {
                let _ = done.send(());
            } else {
                entry.history_waiters.push(done);
                self.read_history(ix, cx);
            }
        }
        ready
    }

    /// Read server `ix`'s stored calls on the blocking pool, once.
    fn read_history(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(entry) = self.servers.get_mut(ix) else {
            return;
        };
        if entry.history_load != HistoryLoad::Unread {
            return;
        }
        entry.history_load = HistoryLoad::Loading;
        let id = entry.record.id.clone();
        let Some(store) = self.store.clone() else {
            self.finish_history_load(&id, Ok(Vec::new()));
            return;
        };
        let server = id.clone();
        self.in_background(
            move || {
                store
                    .list_calls(Some(&server), MAX_HISTORY)
                    .map_err(|e| e.to_string())
            },
            move |state, stored, cx| {
                let stored = stored.unwrap_or_else(|| Err("history task failed".into()));
                state.finish_history_load(&id, stored);
                state.changed(cx);
            },
            cx,
        );
    }

    /// List the stored calls of server `id` beside the calls recorded while
    /// they were read. A failed read is reported and not tried again.
    fn finish_history_load(&mut self, id: &str, stored: Result<Vec<CallRecord>, String>) {
        // The server may have been deleted while its calls were read.
        let Some(pos) = self.servers.iter().position(|s| s.record.id == id) else {
            return;
        };
        let selected = (self.mode == Mode::History && self.selected_server == Some(pos))
            .then(|| self.selected_name())
            .flatten();
        let entry = &mut self.servers[pos];
        entry.history_load = HistoryLoad::Read;
        let waiters = std::mem::take(&mut entry.history_waiters);
        match stored {
            Ok(stored) => {
                let session = std::mem::take(&mut entry.history);
                entry.set_history(persistence::merge_history(session, stored));
            }
            Err(e) => {
                let what = format!("history of `{}` could not be read", entry.record.name);
                self.note_failure(&what, e);
            }
        }
        // The rows shift under a selected call; it stays selected.
        if let Some(name) = selected {
            self.selected_item = self.items().iter().position(|i| i.name == name);
        }
        for waiter in waiters {
            let _ = waiter.send(());
        }
    }

    /// Remember a recorded call at the top of the server's history. The
    /// selected history row stays selected even though the rows shift.
    pub(crate) fn push_history(&mut self, record: CallRecord) {
        let selected = (self.mode == Mode::History)
            .then(|| self.selected_name())
            .flatten();
        if let Some(entry) = self
            .servers
            .iter_mut()
            .find(|s| s.record.id == record.server_id)
        {
            entry.push_call(record);
        }
        if let Some(id) = selected {
            self.selected_item = self.items().iter().position(|i| i.name == id);
        }
    }

    /// Close the change banner of the selected server.
    pub fn dismiss_diff(&mut self, cx: &mut Context<Self>) {
        if let Some(ix) = self.selected_server {
            self.servers[ix].diff_dismissed = true;
            self.changed(cx);
        }
    }

    /// Show or hide the change list of the selected server.
    pub fn toggle_diff_details(&mut self, cx: &mut Context<Self>) {
        if let Some(ix) = self.selected_server {
            self.servers[ix].diff_expanded = !self.servers[ix].diff_expanded;
            self.changed(cx);
        }
    }

    /// The diff to show in the banner, if any.
    pub fn visible_diff(&self) -> Option<&mcp_diff::SnapshotDiff> {
        let server = self.server()?;
        if server.diff_dismissed {
            return None;
        }
        // A comparison with a file is shown whatever it found: it was asked for.
        server.diff.as_ref().filter(|d| {
            server.diff_baseline.is_some() || matches!(d.outcome, mcp_diff::Outcome::Changed { .. })
        })
    }

    // ------------------------------------------------------------ server requests

    /// The request the dialog shows: the oldest unanswered one.
    pub fn current_request(&self) -> Option<&PendingRequest> {
        self.pending.first()
    }

    /// Deliver `response` to the request at the head of the queue.
    pub fn answer_request(&mut self, response: mcp_core::ServerResponse, cx: &mut Context<Self>) {
        if self.pending.is_empty() {
            return;
        }
        let pending = self.pending.remove(0);
        if let (mcp_core::ServerRequestKind::ListRoots, mcp_core::ServerResponse::Roots(roots)) =
            (&pending.request.kind, &response)
        {
            self.keep_roots(&pending.server_id, roots.clone(), false, cx);
        }
        pending.request.respond(response);
        self.changed(cx);
    }
}

/// Names of the items of `fresh` that `current` did not list, or listed in
/// another form.
fn altered<'a, T: PartialEq>(
    current: &'a [T],
    fresh: &'a [T],
    name: impl Fn(&T) -> &String + Copy + 'a,
) -> impl Iterator<Item = String> + 'a {
    fresh
        .iter()
        .filter(move |item| {
            !current
                .iter()
                .any(|old| name(old) == name(item) && old == *item)
        })
        .map(move |item| name(item).clone())
}

/// Whether `row` is at `min` or above. A row without a level (a request,
/// stderr), a level the protocol does not name, and a session separator
/// always pass.
fn level_passes(min: Option<&str>, row: &LogRow) -> bool {
    let rank = |level: &str| mcp_core::LOG_LEVELS.iter().position(|l| *l == level);
    match (min.and_then(rank), row.level.as_deref().and_then(rank)) {
        (Some(min), Some(level)) => row.session_break || level >= min,
        _ => true,
    }
}

/// The sections of the Server view that the snapshot has something for, and
/// Settings, which every server has, connected or not.
fn server_sections(snap: Option<&Snapshot>) -> Vec<Item> {
    let mut sections = Vec::new();
    if let Some(snap) = snap {
        sections.push(Item::named("server", "Server info"));
        sections.push(Item::named("capabilities", "Capabilities"));
        if snap.instructions.is_some() {
            sections.push(Item::named("instructions", "Instructions"));
        }
        sections.push(Item::named("lists", "Lists"));
        sections.push(Item::named("snapshots", "Snapshots"));
        sections.push(Item::named("roots", "Roots"));
    }
    sections.push(Item::named(SETTINGS_SECTION, "Settings"));
    sections
}

/// The keyring entry a spec's auth refers to, if any.
pub fn keyring_id(spec: &ServerSpec) -> Option<String> {
    match spec {
        ServerSpec::Http {
            auth: mcp_core::AuthRef::Bearer { keyring_id } | mcp_core::AuthRef::OAuth { keyring_id },
            ..
        } => Some(keyring_id.clone()),
        _ => None,
    }
}

fn history_item(call: &CallRecord, text: Rc<str>) -> Item {
    Item {
        name: call.id.clone(),
        label: call.name.clone(),
        meta: Some(when_listed(call.at, time::OffsetDateTime::now_utc())),
        failed: call.status != mcp_store::CallStatus::Ok,
        changed: false,
        watched: false,
        text: Some(text),
    }
}

/// Bytes of a call's searchable text kept: a result of many megabytes is
/// matched on its head, so a long history costs a bounded amount to filter.
pub const SEARCH_TEXT_LIMIT: usize = 256 * 1024;

/// Everything of a call the History filter matches, lowercased: its kind,
/// name, outcome, error, arguments and result, in that order, so what is
/// cut at [`SEARCH_TEXT_LIMIT`] is the tail of the result.
fn search_text(call: &CallRecord) -> String {
    let mut text = String::new();
    let mut push = |piece: &str| {
        if text.len() >= SEARCH_TEXT_LIMIT {
            return;
        }
        let room = SEARCH_TEXT_LIMIT - text.len();
        let piece = if piece.len() > room {
            let mut end = room;
            while !piece.is_char_boundary(end) {
                end -= 1;
            }
            &piece[..end]
        } else {
            piece
        };
        text.push_str(piece);
        text.push('\n');
    };
    push(&format!("{:?}", call.kind));
    push(&call.name);
    push(&format!("{:?}", call.status));
    if let Some(error) = &call.error {
        push(error);
    }
    push(&call.args.to_string());
    if let Some(result) = &call.result {
        push(&result.to_string());
    }
    text.to_lowercase()
}

pub(crate) fn demo_record(name: &str, spec: ServerSpec) -> ServerRecord {
    ServerRecord {
        id: uuid::Uuid::now_v7().to_string(),
        name: name.to_owned(),
        spec,
        policy: mcp_core::ServerRequestPolicy::default(),
        protocol: mcp_core::ProtocolMode::default(),
    }
}

/// Human size of a JSON payload.
pub fn size_label(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// The one-line body of a row: a string payload's own text, anything else
/// the compact JSON `head` already printed for it.
fn preview(value: &Value, head: &str) -> String {
    let text = match value {
        Value::Null => "",
        Value::String(s) => s.as_str(),
        _ => head,
    };
    let mut out: String = text.chars().take(200).collect();
    if out.len() < text.len() {
        out.push('…');
    }
    out
}

/// `HH:MM:SS.mmm` in the local zone.
/// A History row's time, in local time: the time of day for a call made
/// today, with the date for an older one, so calls from different days never
/// read as out of order.
pub(crate) fn when_listed(at: time::OffsetDateTime, now: time::OffsetDateTime) -> String {
    let offset = time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC);
    listed_label(at.to_offset(offset), now.to_offset(offset))
}

/// [`when_listed`] for two times already in the same offset.
fn listed_label(at: time::OffsetDateTime, now: time::OffsetDateTime) -> String {
    let formatted = if at.date() == now.date() {
        at.format(time::macros::format_description!(
            "[hour]:[minute]:[second].[subsecond digits:3]"
        ))
    } else if at.year() == now.year() {
        at.format(time::macros::format_description!(
            "[month repr:short] [day padding:none] [hour]:[minute]"
        ))
    } else {
        at.format(time::macros::format_description!(
            "[year]-[month]-[day] [hour]:[minute]"
        ))
    };
    formatted.unwrap_or_default()
}

pub(crate) fn time_label(at: time::OffsetDateTime) -> String {
    let local = time::UtcOffset::current_local_offset()
        .map(|o| at.to_offset(o))
        .unwrap_or(at);
    let format = time::macros::format_description!("[hour]:[minute]:[second].[subsecond digits:3]");
    local.format(&format).unwrap_or_default()
}

/// Everything a server-initiated request carries, as one JSON value. Shown
/// both in the log drawer and, foldable, under the request dialog.
pub fn server_request_payload(kind: &mcp_core::ServerRequestKind) -> Value {
    match kind {
        mcp_core::ServerRequestKind::Sampling(params) => params.clone(),
        mcp_core::ServerRequestKind::Elicitation { message, mode } => match mode {
            mcp_core::ElicitationMode::Form { schema } => {
                serde_json::json!({ "message": message, "requestedSchema": schema })
            }
            mcp_core::ElicitationMode::Url {
                url,
                elicitation_id,
            } => {
                let mut payload = serde_json::json!({ "message": message, "url": url });
                if let Some(id) = elicitation_id {
                    payload["elicitationId"] = Value::String(id.clone());
                }
                payload
            }
        },
        mcp_core::ServerRequestKind::ListRoots => Value::Object(serde_json::Map::new()),
    }
}

/// What one session event brings to the model, worked out on the runtime so
/// the GPUI thread only files it.
#[derive(Debug, Default)]
pub struct Incoming {
    /// The row the log drawer shows for it.
    pub row: Option<LogRow>,
    /// The connection state it reports, with its detail.
    pub change: Option<(ConnectionState, Option<String>)>,
    /// A request the server waits on an answer to.
    pub request: Option<mcp_core::ServerRequest>,
    /// A list the server says changed.
    pub list_changed: Option<ListKind>,
    /// A resource the server says changed.
    pub resource_updated: Option<String>,
    /// Progress of a request of ours.
    pub progress: Option<Progress>,
    /// The id of a request of the server's that it withdrew.
    pub withdrawn: Option<Value>,
}

/// Split `event` into its log row, the state change it reports and the
/// request it carries. Consumes the event: a frame's payload is moved into
/// its row, which keeps only the head of one over [`LOG_PAYLOAD_LIMIT`], and a
/// request's answer slot is moved into the queue rather than copied.
pub fn incoming(event: Event) -> Incoming {
    let at = event.at;
    match event.kind {
        EventKind::StateChange { state, detail } => Incoming {
            row: Some(state_row(at, &state, detail.as_deref())),
            change: Some((state, detail)),
            ..Incoming::default()
        },
        EventKind::ServerRequest(request) => Incoming {
            row: Some(request_row(at, &request)),
            request: Some(request),
            ..Incoming::default()
        },
        // Semantic events whose wire notification is logged on its own.
        EventKind::ListChanged(kind) => Incoming {
            list_changed: Some(kind),
            ..Incoming::default()
        },
        EventKind::ResourceUpdated { uri } => Incoming {
            resource_updated: Some(uri),
            ..Incoming::default()
        },
        EventKind::Progress {
            token,
            progress,
            total,
            message,
        } => Incoming {
            progress: Some(Progress {
                token,
                progress,
                total,
                message,
            }),
            ..Incoming::default()
        },
        EventKind::RequestCancelled { id, .. } => Incoming {
            withdrawn: Some(id),
            ..Incoming::default()
        },
        kind => Incoming {
            row: frame_row(at, kind),
            ..Incoming::default()
        },
    }
}

/// Turn an event into a log row; `None` for events the drawer does not show.
/// Copies the payload; [`incoming`], which the app uses, moves it.
#[cfg(test)]
pub(crate) fn log_row(event: &Event) -> Option<LogRow> {
    incoming(event.clone()).row
}

/// The row a connection state change leaves.
fn state_row(at: time::OffsetDateTime, state: &ConnectionState, detail: Option<&str>) -> LogRow {
    LogRow::new(
        at,
        Dir::Note,
        format!("{state:?}").to_lowercase(),
        Value::String(detail.unwrap_or_default().into()),
        matches!(state, ConnectionState::Failed),
    )
}

/// The row a server-initiated request leaves. The log carries what the server
/// actually asked for, so the expanded row is a real payload rather than a
/// placeholder sentence.
fn request_row(at: time::OffsetDateTime, request: &mcp_core::ServerRequest) -> LogRow {
    let mut row = LogRow::new(
        at,
        Dir::In,
        request.kind.method().into(),
        server_request_payload(&request.kind),
        false,
    );
    row.frame = Some(mcp_exchange::Frame::Request);
    row
}

/// The row of any other event, built from its owned payload; `None` for
/// events the drawer does not show.
fn frame_row(at: time::OffsetDateTime, kind: EventKind) -> Option<LogRow> {
    // A row that was a JSON-RPC frame remembers which one and its id, so the
    // log can hand back the whole message rather than the one field it shows.
    let frame = |dir, method, payload, is_error, f, id| {
        let mut row = LogRow::new(at, dir, method, payload, is_error);
        row.frame = Some(f);
        row.id = id;
        row
    };
    let dir_of = |d: Direction| match d {
        Direction::Outbound => Dir::Out,
        Direction::Inbound => Dir::In,
    };
    Some(match kind {
        EventKind::Request {
            direction,
            id,
            method,
            params,
        } => frame(
            dir_of(direction),
            method,
            params.unwrap_or(Value::Null),
            false,
            mcp_exchange::Frame::Request,
            Some(id),
        ),
        EventKind::Response {
            direction,
            id,
            method,
            result,
            ..
        } => frame(
            dir_of(direction),
            method.unwrap_or_else(|| "response".into()),
            result,
            false,
            mcp_exchange::Frame::Response,
            Some(id),
        ),
        EventKind::Error {
            direction,
            id,
            method,
            code,
            message,
            data,
            ..
        } => frame(
            dir_of(direction),
            method.unwrap_or_else(|| "error".into()),
            serde_json::json!({"code": code, "message": message, "data": data}),
            true,
            mcp_exchange::Frame::Error,
            id,
        ),
        EventKind::Notification { method, params, .. } => {
            let payload = params.unwrap_or(Value::Null);
            // A server's own log message carries its severity: an error
            // passes the errors filter and is drawn as one.
            let level = (method == "notifications/message")
                .then(|| {
                    payload
                        .get("level")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .flatten();
            let is_error = level
                .as_deref()
                .is_some_and(|l| matches!(l, "error" | "critical" | "alert" | "emergency"));
            let mut row = frame(
                Dir::Note,
                method,
                payload,
                is_error,
                mcp_exchange::Frame::Notification,
                None,
            );
            if let Some(level) = level {
                row.body = format!("{level} · {}", row.body);
                row.bytes += level.len() + 3;
                row.level = Some(level);
            }
            row
        }
        EventKind::Stderr { line } => {
            LogRow::new(at, Dir::Note, "stderr".into(), Value::String(line), false)
        }
        EventKind::StateChange { state, detail } => state_row(at, &state, detail.as_deref()),
        EventKind::ServerRequest(request) => request_row(at, &request),
        EventKind::Note {
            topic,
            text,
            is_error,
        } => LogRow::new(at, Dir::Note, topic, Value::String(text), is_error),
        // Semantic duplicates of wire notifications already logged above.
        EventKind::ListChanged(_)
        | EventKind::ResourceUpdated { .. }
        | EventKind::Log { .. }
        | EventKind::Progress { .. }
        | EventKind::RequestCancelled { .. } => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn demo() -> AppState {
        let mut state = AppState::new(None, None);
        let spec = ServerSpec::Stdio {
            command: "weather".into(),
            args: vec![],
            env: Default::default(),
            cwd: None,
        };
        let mut entry = ServerEntry::new(demo_record("weather", spec));
        entry.status = Status::Connected;
        entry.set_snapshot(Some(
            serde_json::from_value(json!({
                "protocolVersion": "2025-11-25",
                "serverInfo": {"name": "weather", "version": "0.4.1"},
                "capabilities": {"tools": {}, "resources": {}, "prompts": {}},
                "tools": [
                    {"name": "get_weather", "inputSchema": {"type": "object"}},
                    {"name": "get_forecast", "inputSchema": {"type": "object"}},
                    {"name": "search_city", "inputSchema": {"type": "object"}}
                ],
                "resources": [{"uri": "weather://stations", "name": "stations"}],
                "resourceTemplates": [{"uriTemplate": "weather://city/{name}", "name": "city"}],
                "prompts": [{"name": "brief"}],
                "takenAt": "2026-09-11T12:00:00Z"
            }))
            .unwrap(),
        ));
        entry.last_call_ms = Some(142);
        state.servers.push(entry);
        state.selected_server = Some(0);
        state
    }

    #[test]
    fn status_text_matches_the_design_format() {
        let state = demo();
        assert_eq!(
            state.status_text(false),
            "weather · Connected · Last call 142 ms"
        );
        assert_eq!(AppState::new(None, None).status_text(false), "No server");
        // A failure shown under the settings is not repeated; otherwise the
        // line gives the reason.
        let mut state = demo();
        state.servers[0].status = Status::Error("The server ended the session.".into());
        assert_eq!(state.status_text(true), "weather · Error");
        assert_eq!(
            state.status_text(false),
            "weather · Error · The server ended the session."
        );
    }

    #[test]
    fn a_connect_failure_is_explained_by_its_result_not_its_state_change() {
        let mut state = demo();
        let id = state.servers[0].record.id.clone();
        let sink = EventSink::new(8);
        let failed = |sink: &EventSink| {
            vec![incoming(sink.emit(EventKind::StateChange {
                state: ConnectionState::Failed,
                detail: Some("Send message error Transport [a::B<c::D>] error: refused".into()),
            }))]
        };
        // Connecting, the result says why; the event that races it only logs.
        state.servers[0].status = Status::Connecting;
        state.apply_events(&id, 0, failed(&sink));
        assert_eq!(state.servers[0].status, Status::Connecting);
        assert_eq!(state.servers[0].log.last().unwrap().method, "failed");
        // A session that was up fails here, in the pane's words.
        state.servers[0].status = Status::Connected;
        state.apply_events(&id, 0, failed(&sink));
        assert_eq!(state.servers[0].status, Status::Error("refused.".into()));
    }

    #[test]
    fn a_refusal_is_forgotten_by_the_next_failure_of_another_kind() {
        let mut state = demo();
        let entry = &mut state.servers[0];
        entry.connect_failed(&mcp_core::Error::AuthRequired {
            challenge: Some("Bearer realm=\"x\"".into()),
        });
        assert!(entry.unauthorized);
        entry.connect_failed(&mcp_core::Error::Transport("connection refused".into()));
        assert!(!entry.unauthorized, "not a refusal");
        assert!(matches!(entry.status, Status::Error(_)));
        assert_eq!(
            entry.auth_challenge.as_deref(),
            Some("Bearer realm=\"x\""),
            "the next authorization still starts from the challenge"
        );
    }

    #[test]
    fn the_settings_pane_is_the_selected_server_off_or_failed_outside_history() {
        let mut state = demo();
        assert_eq!(state.settings_pane(), None, "connected");
        assert!(state.list_opens());
        state.servers[0].status = Status::Connecting;
        assert_eq!(state.settings_pane(), None, "connecting");
        assert!(!state.list_opens(), "nothing to open yet");
        for status in [Status::Off, Status::Error("gone".into())] {
            state.servers[0].status = status;
            assert_eq!(state.settings_pane(), Some(0));
            assert!(!state.list_opens());
        }
        state.mode = Mode::History;
        assert_eq!(state.settings_pane(), None, "History stays readable");
        assert!(state.list_opens());
    }

    #[test]
    fn a_list_that_failed_is_named_where_it_is_missing() {
        let mut state = demo();
        let mut snapshot = state.servers[0].snapshot().unwrap().clone();
        snapshot.list_failures.push(ListFailure {
            method: "resources/templates/list".into(),
            error: "request timed out after 60s".into(),
        });
        state.servers[0].set_snapshot(Some(snapshot));
        assert_eq!(
            state.status_text(false),
            "weather · Connected · resources/templates/list failed · Last call 142 ms"
        );
        let failures = state.list_failures(Mode::Resources);
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].error, "request timed out after 60s");
        for mode in [Mode::Tools, Mode::Prompts, Mode::History] {
            assert!(state.list_failures(mode).is_empty(), "{mode:?}");
        }
        assert_eq!(Mode::History.list_methods(), &[] as &[&str]);
    }

    #[test]
    fn filter_and_selection() {
        let mut state = demo();
        assert_eq!(state.list_count(), "3/3");
        state.filter = "get".into();
        assert_eq!(state.list_count(), "2/3");
        state.selected_item = Some(1);
        assert_eq!(state.selected_name().as_deref(), Some("get_forecast"));
        assert!(state.selected_tool().is_some());
        state.mode = Mode::Resources;
        assert_eq!(state.count(Mode::Resources), Some(2));
        state.filter.clear();
        state.selected_item = Some(1);
        assert!(state.selected_template().is_some());
    }

    #[test]
    fn the_history_filter_matches_a_calls_whole_content() {
        let mut state = demo();
        let server_id = state.servers[0].record.id.clone();
        let record =
            |id: &str, name: &str, args: Value, result: Option<Value>, error: Option<&str>| {
                CallRecord {
                    id: id.into(),
                    server_id: server_id.clone(),
                    kind: mcp_store::CallKind::Tool,
                    name: name.into(),
                    args,
                    result,
                    status: if error.is_some() {
                        mcp_store::CallStatus::Failed
                    } else {
                        mcp_store::CallStatus::Ok
                    },
                    error: error.map(str::to_owned),
                    elapsed_ms: 1,
                    at: time::OffsetDateTime::UNIX_EPOCH,
                }
            };
        state.push_history(record(
            "c0",
            "get_weather",
            json!({"city": "Lisbon"}),
            Some(json!({"content": [{"type": "text", "text": "Cloudy, 19 °C"}]})),
            None,
        ));
        state.push_history(record(
            "c1",
            "get_weather",
            json!({"city": "Oslo"}),
            None,
            Some("timed out after 60 s"),
        ));
        state.mode = Mode::History;
        let matching = |state: &mut AppState, needle: &str| {
            state.filter = needle.into();
            state
                .items()
                .iter()
                .map(|i| i.name.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            matching(&mut state, "weather"),
            ["c1", "c0"],
            "the name still matches"
        );
        assert_eq!(
            matching(&mut state, "lisbon"),
            ["c0"],
            "an argument value, any case"
        );
        assert_eq!(
            matching(&mut state, "cloudy"),
            ["c0"],
            "text inside the result"
        );
        assert_eq!(
            matching(&mut state, "timed out"),
            ["c1"],
            "the error of a failed call"
        );
        assert_eq!(matching(&mut state, "failed"), ["c1"], "the outcome");
        assert!(matching(&mut state, "helsinki").is_empty());

        // Clearing the calls drops their text with them.
        state.servers[0].set_history(Vec::new());
        assert!(matching(&mut state, "").is_empty());
        assert!(state.servers[0].search_text.borrow().is_empty());
    }

    #[test]
    fn deleting_one_call_leaves_the_others() {
        let mut state = demo();
        let server_id = state.servers[0].record.id.clone();
        for i in 0..3 {
            state.push_history(CallRecord {
                id: format!("c{i}"),
                server_id: server_id.clone(),
                kind: mcp_store::CallKind::Tool,
                name: format!("tool{i}"),
                args: json!({}),
                result: None,
                status: mcp_store::CallStatus::Ok,
                error: None,
                elapsed_ms: 1,
                at: time::OffsetDateTime::UNIX_EPOCH,
            });
        }
        state.mode = Mode::History;
        // Newest first: row 1 is c1.
        state.selected_item = Some(1);
        assert_eq!(state.selected_call().unwrap().id, "c1");
        use gpui_kit::AppContext as _;
        let mut cx = gpui_kit::TestAppContext::single();
        let entity = cx.new(|_| state);
        entity.update(&mut cx, |state, cx| {
            state.delete_selected_call(cx);
            let ids: Vec<_> = state.servers[0]
                .history()
                .iter()
                .map(|c| c.id.as_str())
                .collect();
            assert_eq!(ids, ["c2", "c0"]);
            assert_eq!(
                state.selected_item, None,
                "the deleted call is no longer selected"
            );
            assert_eq!(state.count(Mode::History), Some(2));
            // Nothing selected: nothing to delete, nothing changes.
            state.delete_selected_call(cx);
            assert_eq!(state.servers[0].history().len(), 2);
        });
    }

    #[test]
    fn a_calls_search_text_is_bounded() {
        let big = "x".repeat(SEARCH_TEXT_LIMIT * 2);
        let call = CallRecord {
            id: "c".into(),
            server_id: "s".into(),
            kind: mcp_store::CallKind::Resource,
            name: "file:///big".into(),
            args: json!({}),
            result: Some(json!({"text": big})),
            status: mcp_store::CallStatus::Ok,
            error: None,
            elapsed_ms: 1,
            at: time::OffsetDateTime::UNIX_EPOCH,
        };
        let text = search_text(&call);
        assert!(text.len() <= SEARCH_TEXT_LIMIT + 8, "{}", text.len());
        assert!(text.starts_with("resource\nfile:///big\nok\n"));
    }

    #[test]
    fn history_mode_lists_calls_newest_first() {
        let mut state = demo();
        for (i, status) in [mcp_store::CallStatus::Ok, mcp_store::CallStatus::Failed]
            .into_iter()
            .enumerate()
        {
            state.push_history(CallRecord {
                id: format!("c{i}"),
                server_id: state.servers[0].record.id.clone(),
                kind: mcp_store::CallKind::Tool,
                name: format!("tool{i}"),
                args: json!({}),
                result: None,
                status,
                error: None,
                elapsed_ms: 1,
                at: time::OffsetDateTime::UNIX_EPOCH,
            });
        }
        state.mode = Mode::History;
        assert_eq!(state.count(Mode::History), Some(2));
        let items = state.items();
        assert_eq!(items[0].label, "tool1");
        assert!(items[0].failed);
        assert!(items[0].meta.is_some());
        state.selected_item = Some(1);
        assert_eq!(state.selected_call().unwrap().name, "tool0");
        state.push_history(CallRecord {
            id: "c2".into(),
            server_id: state.servers[0].record.id.clone(),
            kind: mcp_store::CallKind::Tool,
            name: "tool2".into(),
            args: json!({}),
            result: None,
            status: mcp_store::CallStatus::Ok,
            error: None,
            elapsed_ms: 1,
            at: time::OffsetDateTime::UNIX_EPOCH,
        });
        assert_eq!(state.selected_item, Some(2), "selection follows the row");
        assert_eq!(state.selected_call().unwrap().name, "tool0");
        state.filter = "tool0".into();
        assert_eq!(state.list_count(), "1/3");
    }

    #[test]
    fn history_is_not_read_at_startup() {
        let store = Store::open_in_memory().unwrap();
        let policy = mcp_core::ServerRequestPolicy::default();
        let record = store
            .add_server("weather", &stdio("weather"), &policy)
            .unwrap();
        for name in ["first", "second", "third"] {
            store
                .record_call(mcp_store::NewCall {
                    server_id: record.id.clone(),
                    kind: mcp_store::CallKind::Tool,
                    name: name.into(),
                    args: json!({}),
                    result: None,
                    status: mcp_store::CallStatus::Ok,
                    error: None,
                    elapsed_ms: 1,
                })
                .unwrap();
        }
        let mut state = AppState::new(None, Some(store.clone()));
        assert!(
            state.servers[0].history().is_empty(),
            "no call is read before History is shown"
        );
        assert_eq!(state.servers[0].history_load(), HistoryLoad::Unread);
        state.servers[0].set_snapshot(demo().servers[0].snapshot().cloned());
        state.mode = Mode::History;
        assert_eq!(state.count(Mode::History), None, "no number while unread");
        assert!(state.history_reading());
        assert_eq!(state.list_count(), "", "nor in the filter row");

        // A call recorded this session while the stored ones are read, and
        // selected.
        state.push_history(CallRecord {
            id: "session".into(),
            server_id: record.id.clone(),
            kind: mcp_store::CallKind::Tool,
            name: "session".into(),
            args: json!({}),
            result: None,
            status: mcp_store::CallStatus::Ok,
            error: None,
            elapsed_ms: 1,
            at: time::OffsetDateTime::UNIX_EPOCH,
        });
        state.selected_item = Some(0);
        let stored = store.list_calls(Some(&record.id), MAX_HISTORY).unwrap();
        state.finish_history_load("deleted meanwhile", Ok(Vec::new()));
        state.finish_history_load(&record.id, Ok(stored.clone()));

        assert_eq!(state.servers[0].history_load(), HistoryLoad::Read);
        assert!(!state.history_reading());
        let mut expected: Vec<_> = stored.iter().map(|c| c.id.as_str()).collect();
        expected.push("session");
        let listed: Vec<_> = state.items().iter().map(|i| i.name.clone()).collect();
        assert_eq!(listed, expected, "newest first, the session's call kept");
        assert_eq!(state.count(Mode::History), Some(4));
        assert_eq!(state.list_count(), "4/4");
        assert_eq!(
            state.selected_name().as_deref(),
            Some("session"),
            "the selection follows its row"
        );
        // A stored call whose answer is filed after the read is not listed twice.
        state.push_history(stored[0].clone());
        assert_eq!(state.count(Mode::History), Some(4));

        // A read that fails is reported and not tried again.
        state.servers[0].history_load = HistoryLoad::Loading;
        state.finish_history_load(&record.id, Err("disk I/O error".into()));
        assert_eq!(state.servers[0].history_load(), HistoryLoad::Read);
        assert_eq!(state.count(Mode::History), Some(4), "what was listed stays");
        assert_eq!(
            state.persistence,
            Persistence::Failed("history of `weather` could not be read: disk I/O error".into())
        );
    }

    #[test]
    fn the_item_list_is_built_once_until_its_inputs_change() {
        let mut state = demo();
        let first = state.items();
        assert!(
            Rc::ptr_eq(&first, &state.items()),
            "a second read shares it"
        );
        assert_eq!(state.list_count(), "3/3");
        assert!(Rc::ptr_eq(&first, &state.items()), "so does the count");

        state.filter = "get".into();
        let filtered = state.items();
        assert!(!Rc::ptr_eq(&first, &filtered), "the filter rebuilds");
        assert_eq!(filtered.len(), 2);

        let mut snapshot = state.servers[0].snapshot().unwrap().clone();
        snapshot.tools.retain(|t| &*t.name != "get_forecast");
        state.servers[0].set_snapshot(Some(snapshot));
        let rebuilt = state.items();
        assert!(!Rc::ptr_eq(&filtered, &rebuilt), "a new snapshot rebuilds");
        assert_eq!(rebuilt.len(), 1);

        let server_id = state.servers[0].record.id.clone();
        let call = |id: &str| CallRecord {
            id: id.into(),
            server_id: server_id.clone(),
            kind: mcp_store::CallKind::Tool,
            name: id.into(),
            args: json!({}),
            result: None,
            status: mcp_store::CallStatus::Ok,
            error: None,
            elapsed_ms: 1,
            at: time::OffsetDateTime::UNIX_EPOCH,
        };
        state.filter.clear();
        state.mode = Mode::History;
        state.push_history(call("c0"));
        state.push_history(call("c1"));
        state.selected_item = Some(1);
        let history = state.items();
        assert_eq!(history.len(), 2);
        state.push_history(call("c2"));
        assert!(!Rc::ptr_eq(&history, &state.items()), "a call rebuilds");
        assert_eq!(state.selected_name().as_deref(), Some("c0"));

        let spec = ServerSpec::Stdio {
            command: "files".into(),
            args: vec![],
            env: Default::default(),
            cwd: None,
        };
        let listed = state.items();
        state.selected_server = Some(state.add_demo_server("files", spec, Status::Off));
        let other = state.items();
        assert!(!Rc::ptr_eq(&listed, &other), "another server rebuilds");
        assert!(other.is_empty());
    }

    #[test]
    fn delete_needs_confirmation() {
        let mut state = demo();
        assert!(state.confirm.is_none());
        // Model-only checks: the context-taking methods run in the headless UI test.
        state.confirm = state.selected_server.map(Confirm::Delete);
        assert_eq!(state.confirm, Some(Confirm::Delete(0)));
        state.confirm = None;
        assert_eq!(state.servers.len(), 1, "cancel keeps the server");
    }

    #[test]
    fn diff_banner_visibility() {
        let mut state = demo();
        assert!(state.visible_diff().is_none());
        let snap = state.servers[0].snapshot().unwrap().clone();
        state.servers[0].diff = Some(mcp_diff::diff(None, &snap));
        assert!(
            state.visible_diff().is_none(),
            "first snapshot has no banner"
        );
        let mut other = snap.clone();
        other.tools.pop();
        state.servers[0].diff = Some(mcp_diff::diff(Some(&snap), &other));
        assert!(state.visible_diff().unwrap().has_breaking());
        state.servers[0].diff_dismissed = true;
        assert!(state.visible_diff().is_none());
    }

    #[test]
    fn log_rows_from_events() {
        let sink = EventSink::new(8);
        let event = sink.emit(EventKind::Request {
            direction: Direction::Outbound,
            id: json!(1),
            method: "tools/list".into(),
            params: Some(json!({})),
        });
        let row = log_row(&event).unwrap();
        assert_eq!(row.dir, Dir::Out);
        assert_eq!(row.method, "tools/list");
        assert_eq!(row.body, "{}");
        assert_eq!(row.size, "2 B");
        assert_eq!(row.time.len(), 12);
        assert!(!row.session_break, "a wire row is not a separator");
        let skipped = sink.emit(EventKind::ListChanged(mcp_core::ListKind::Tools));
        assert!(log_row(&skipped).is_none());
        assert_eq!(size_label(2150), "2.1 KB");
    }

    #[test]
    fn a_session_break_is_a_note_every_filter_shows() {
        let row = LogRow::session_break(time::OffsetDateTime::UNIX_EPOCH);
        assert_eq!(row.dir, Dir::Note);
        assert_eq!(row.method, "session");
        assert!(!row.is_error);
        assert!(row.message().is_none(), "it was never on the wire");
        for filter in LogFilter::ALL {
            assert!(filter.accepts(&row), "{filter:?} hides the separator");
        }
    }

    fn stderr_row(line: &str) -> LogRow {
        let sink = EventSink::new(8);
        log_row(&sink.emit(EventKind::Stderr { line: line.into() })).unwrap()
    }

    #[test]
    fn a_new_session_keeps_the_rows_of_the_last() {
        let mut state = demo();
        let entry = &mut state.servers[0];
        let at = time::OffsetDateTime::UNIX_EPOCH;
        entry.break_log(at);
        assert!(entry.log.is_empty(), "nothing to separate from");
        entry.push_log(stderr_row("unknown argument `--bogus`"));
        entry.break_log(at);
        entry.break_log(at);
        assert_eq!(entry.log.len(), 2, "one separator per session that logged");
        assert_eq!(entry.log[0].body, "unknown argument `--bogus`");
        assert!(entry.log[1].session_break);
        entry.push_log(stderr_row("again"));
        entry.break_log(at);
        assert_eq!(entry.log.len(), 4);
        assert!(entry.log[3].session_break);
    }

    #[test]
    fn the_log_cap_drops_the_oldest_rows_first() {
        let mut state = demo();
        let entry = &mut state.servers[0];
        let row = stderr_row("");
        for i in 0..MAX_LOG_ROWS {
            let mut row = row.clone();
            row.body = i.to_string();
            entry.push_log(row);
        }
        assert_eq!(entry.first_log_id(), 0);
        assert_eq!(entry.break_log(time::OffsetDateTime::UNIX_EPOCH), 1);
        assert_eq!(entry.log.len(), MAX_LOG_ROWS);
        assert_eq!(entry.log[0].body, "1", "the oldest row made room");
        assert_eq!(entry.log[0].row_id, 1, "ids do not shift");
        assert_eq!(entry.first_log_id(), 1);
        assert!(entry.log[MAX_LOG_ROWS - 1].session_break);
    }

    #[test]
    fn visible_log_names_rows_without_copying_them() {
        let mut state = demo();
        let sink = EventSink::new(8);
        let request = sink.emit(EventKind::Request {
            direction: Direction::Outbound,
            id: json!(1),
            method: "tools/list".into(),
            params: None,
        });
        let note = sink.emit(EventKind::Notification {
            direction: Direction::Inbound,
            method: "notifications/message".into(),
            params: None,
        });
        let entry = &mut state.servers[0];
        entry.push_log(stderr_row("booting"));
        entry.push_log(log_row(&request).unwrap());
        entry.push_log(LogRow::session_break(time::OffsetDateTime::UNIX_EPOCH));
        entry.push_log(log_row(&note).unwrap());
        let shown = [
            (LogFilter::All, vec![0, 1, 2, 3]),
            (LogFilter::Requests, vec![1, 2]),
            (LogFilter::Notifications, vec![2, 3]),
            (LogFilter::Errors, vec![2]),
            (LogFilter::Stderr, vec![0, 2]),
        ];
        for (filter, rows) in shown {
            state.log_filter = filter;
            assert_eq!(state.visible_log(), rows, "{filter:?}");
        }
        state.selected_server = None;
        assert!(state.visible_log().is_empty());
    }

    #[test]
    fn a_row_is_found_by_its_id_on_its_own_server_while_kept() {
        let mut state = demo();
        let other = state.add_demo_server("other", stdio("other"), Status::Off);
        for entry in &mut state.servers {
            entry.push_log(stderr_row("first"));
            entry.push_log(stderr_row("second"));
        }
        let id = state.servers[0].record.id.clone();
        state.servers[0].clear_log();
        state.servers[0].push_log(stderr_row("third"));
        let (row, spec) = state.log_row_by_id(&id, 2).unwrap();
        assert_eq!((row.body.as_str(), spec), ("third", &stdio("weather")));
        assert!(state.log_row_by_id(&id, 1).is_none(), "cleared");
        assert!(state.log_row_by_id(&id, 3).is_none(), "not logged yet");
        let other_id = state.servers[other].record.id.clone();
        let (row, _) = state.log_row_by_id(&other_id, 1).unwrap();
        assert_eq!(row.body, "second", "ids are per server");
        assert!(state.log_row_by_id("gone", 0).is_none());
    }

    #[test]
    fn a_payload_over_the_limit_keeps_its_head_and_its_size() {
        let sink = EventSink::new(8);
        let result =
            json!({"content": [{"type": "text", "text": "x".repeat(LOG_PAYLOAD_LIMIT * 2)}]});
        let total = result.to_string().len();
        let event = sink.emit(EventKind::Response {
            direction: Direction::Inbound,
            id: json!(7),
            method: Some("tools/call".into()),
            result,
            elapsed: None,
        });
        let row = log_row(&event).unwrap();
        assert_eq!(row.truncated, Some(total));
        assert_eq!(
            row.size,
            size_label(total),
            "the size column is the original"
        );
        let prefix = format!("truncated {} · {{\"content\"", size_label(total));
        assert!(row.body.starts_with(&prefix), "{}", row.body);
        assert!(row.body.chars().count() < 250, "the body is still one line");
        assert_eq!(row.payload["originalBytes"], total);
        assert_eq!(
            row.payload["truncated"],
            format!("{} payload, first 64.0 KB kept", size_label(total))
        );
        let head = row.payload["head"].as_str().unwrap();
        assert_eq!(head.len(), LOG_PAYLOAD_LIMIT);
        assert!(head.starts_with("{\"content\":[{\"type\":\"text\""));
        assert!(row.bytes < LOG_PAYLOAD_LIMIT + 1024, "{}", row.bytes);
        let message = row.message().unwrap();
        assert_eq!(message["id"], json!(7), "the frame is still the frame");
        assert_eq!(message["result"]["originalBytes"], total);

        let small = stderr_row("ok");
        assert_eq!((small.truncated, &small.payload), (None, &json!("ok")));
        assert_eq!(small.bytes, "\"ok\"".len() + "ok".len() + "stderr".len());
    }

    #[test]
    fn the_log_byte_budget_drops_the_oldest_rows() {
        let mut state = demo();
        let entry = &mut state.servers[0];
        let big = stderr_row(&"x".repeat(60 * 1024));
        assert!(big.truncated.is_none() && big.bytes > 60 * 1024);
        let fits = LOG_BYTE_BUDGET / big.bytes;
        let mut dropped = 0;
        for i in 0..fits + 10 {
            let mut row = big.clone();
            row.time = i.to_string();
            dropped += entry.push_log(row);
            assert!(entry.log_bytes <= LOG_BYTE_BUDGET, "row {i}");
        }
        assert_eq!(dropped, 10);
        assert_eq!(entry.log.len(), fits);
        assert!(
            fits < MAX_LOG_ROWS,
            "the budget, not the row cap, dropped them"
        );
        assert_eq!(entry.first_log_id(), 10);
        assert_eq!(entry.log_bytes, fits * big.bytes);
        assert_eq!(entry.log[0].time, "10", "the oldest went first");
        let newest = (fits + 9).to_string();
        assert_eq!(entry.log.last().unwrap().time, newest, "the newest stays");
        entry.clear_log();
        assert_eq!(
            (entry.log_bytes, entry.first_log_id()),
            (0, fits as u64 + 10)
        );
        entry.push_log(stderr_row("after"));
        assert_eq!(entry.log_bytes, entry.log[0].bytes);
    }

    #[test]
    fn an_event_splits_into_its_row_and_its_change() {
        let sink = EventSink::new(8);
        let failed = incoming(sink.emit(EventKind::StateChange {
            state: ConnectionState::Failed,
            detail: Some("exit status 2".into()),
        }));
        assert_eq!(
            failed.change,
            Some((ConnectionState::Failed, Some("exit status 2".into())))
        );
        assert!(failed.request.is_none());
        let row = failed.row.unwrap();
        assert_eq!((row.method.as_str(), row.is_error), ("failed", true));

        let stderr = incoming(sink.emit(EventKind::Stderr {
            line: "listening".into(),
        }));
        assert!(stderr.change.is_none() && stderr.request.is_none());
        assert_eq!(stderr.row.unwrap().body, "listening");

        let skipped = incoming(sink.emit(EventKind::ListChanged(mcp_core::ListKind::Tools)));
        assert!(skipped.row.is_none() && skipped.change.is_none() && skipped.request.is_none());
    }

    #[test]
    fn a_moved_payload_makes_the_same_row_as_a_borrowed_one() {
        let sink = EventSink::new(8);
        let big = json!({"blob": "x".repeat(LOG_PAYLOAD_LIMIT * 2)});
        let events = [
            EventKind::Request {
                direction: Direction::Outbound,
                id: json!(1),
                method: "tools/call".into(),
                params: Some(big.clone()),
            },
            EventKind::Response {
                direction: Direction::Inbound,
                id: json!(1),
                method: None,
                result: big,
                elapsed: None,
            },
            EventKind::Error {
                direction: Direction::Inbound,
                id: None,
                method: Some("tools/call".into()),
                code: -32602,
                message: "bad".into(),
                data: None,
                elapsed: None,
            },
            EventKind::Notification {
                direction: Direction::Inbound,
                method: "notifications/progress".into(),
                params: None,
            },
        ];
        for kind in events {
            let event = sink.emit(kind);
            let borrowed = log_row(&event).unwrap();
            let moved = incoming(event).row.unwrap();
            assert_eq!(
                (&moved.method, &moved.body, &moved.payload, moved.truncated),
                (
                    &borrowed.method,
                    &borrowed.body,
                    &borrowed.payload,
                    borrowed.truncated
                )
            );
            assert_eq!(
                (moved.frame, &moved.id, moved.is_error, moved.bytes),
                (
                    borrowed.frame,
                    &borrowed.id,
                    borrowed.is_error,
                    borrowed.bytes
                )
            );
        }
    }

    #[test]
    fn the_expanded_log_row_keeps_its_id_until_its_row_leaves() {
        let mut state = demo();
        let id = state.servers[0].record.id.clone();
        let row = stderr_row("");
        for i in 0..MAX_LOG_ROWS {
            let mut row = row.clone();
            row.body = i.to_string();
            assert_eq!(state.append_log(0, row), None, "under the cap");
        }
        let ids: Vec<u64> = state.servers[0].log.iter().map(|r| r.row_id).collect();
        assert_eq!(ids, (0..MAX_LOG_ROWS as u64).collect::<Vec<_>>());
        state.expanded_log = [10].into();
        assert_eq!(state.append_log(0, stderr_row("newest")), Some(1));
        assert_eq!(
            state.expanded_log,
            [10].into(),
            "an older row left, not this one"
        );
        assert_eq!(
            state.servers[0].log[9].row_id, 10,
            "a new index, the same id"
        );
        assert_eq!(state.log_row_by_id(&id, 10).unwrap().0.body, "10");

        state.expanded_log = [1, 10].into();
        assert_eq!(state.append_log(0, stderr_row("newer")), Some(2));
        assert_eq!(
            state.expanded_log,
            [10].into(),
            "the row that left is forgotten, the other stays open"
        );

        // Clear empties the log, and ids carry on rather than start again.
        let next = state.servers[0].next_log_id();
        assert_eq!(next, MAX_LOG_ROWS as u64 + 2);
        state.servers[0].clear_log();
        assert_eq!(state.servers[0].first_log_id(), next);
        state.append_log(0, stderr_row("after"));
        assert_eq!(state.servers[0].log[0].row_id, next);

        // Another server's log does not touch the selected one's expanded row,
        // even once that server's ids have passed it.
        let other = state.add_demo_server("other", stdio("other"), Status::Off);
        state.expanded_log = [next].into();
        let row = stderr_row("other");
        for _ in 0..next as usize + MAX_LOG_ROWS + 1 {
            state.append_log(other, row.clone());
        }
        assert!(state.servers[other].first_log_id() > next);
        assert_eq!(state.expanded_log, [next].into());
    }

    #[test]
    fn a_burst_of_events_is_one_change() {
        let mut state = demo();
        let id = state.servers[0].record.id.clone();
        let sink = EventSink::new(64);
        let lines = |range: std::ops::Range<usize>| -> Vec<Incoming> {
            range
                .map(|i| {
                    incoming(sink.emit(EventKind::Stderr {
                        line: format!("line {i}"),
                    }))
                })
                .collect()
        };
        let applied = state.apply_events(&id, 0, lines(0..50));
        assert_eq!(
            applied,
            Applied {
                changed: true,
                rows_left_before: None,
                ..Applied::default()
            }
        );
        assert_eq!(state.servers[0].log.len(), 50);
        assert_eq!(state.servers[0].log[49].body, "line 49", "in arrival order");

        // A replaced connection, or a deleted server, files nothing.
        assert_eq!(state.apply_events(&id, 1, lines(0..50)), Applied::default());
        assert_eq!(
            state.apply_events("gone", 0, lines(0..50)),
            Applied::default()
        );
        assert_eq!(state.servers[0].log.len(), 50);

        // A state change and the rows behind it apply in order.
        let mut batch = vec![incoming(sink.emit(EventKind::StateChange {
            state: ConnectionState::Failed,
            detail: Some("exit status 1".into()),
        }))];
        batch.extend(lines(50..52));
        assert!(state.apply_events(&id, 0, batch).changed);
        assert_eq!(
            state.servers[0].status,
            Status::Error("exit status 1.".into())
        );
        let methods: Vec<&str> = state.servers[0].log[50..]
            .iter()
            .map(|r| r.method.as_str())
            .collect();
        assert_eq!(methods, ["failed", "stderr", "stderr"]);

        // Rows a batch pushes past the cap are reported once, and close the
        // expanded row when it was one of them.
        state.expanded_log = [1].into();
        let applied = state.apply_events(&id, 0, lines(0..MAX_LOG_ROWS));
        assert_eq!(state.servers[0].first_log_id(), 53, "53 rows made room");
        assert_eq!(applied.rows_left_before, Some(53));
        assert!(state.expanded_log.is_empty());
    }

    #[test]
    fn rows_a_burst_pushes_out_are_announced_to_the_views() {
        use gpui_kit::AppContext as _;

        let mut cx = gpui::TestAppContext::single();
        let state = cx.new(|_| demo());
        let id = cx.read(|cx| state.read(cx).servers[0].record.id.clone());
        let seen = Rc::new(RefCell::new(Vec::new()));
        let heard = seen.clone();
        let _listening = cx.update(|cx| {
            cx.subscribe(&state, move |_, gone: &Gone, _| {
                heard.borrow_mut().push(gone.clone())
            })
        });
        let sink = EventSink::new(64);
        let lines = |count: usize| -> Vec<Incoming> {
            (0..count)
                .map(|i| {
                    incoming(sink.emit(EventKind::Stderr {
                        line: format!("line {i}"),
                    }))
                })
                .collect()
        };

        state.update(&mut cx, |s, cx| s.push_events(&id, 0, lines(10), cx));
        assert!(seen.borrow().is_empty(), "no row left under the cap");

        state.update(&mut cx, |s, cx| {
            s.push_events(&id, 0, lines(MAX_LOG_ROWS), cx)
        });
        assert_eq!(
            *seen.borrow(),
            [Gone::LogRows {
                server_id: id,
                before: 10,
            }],
            "one announcement for the whole batch"
        );
    }

    #[test]
    fn a_deleted_server_takes_its_responses_and_names_its_calls() {
        let mut state = demo();
        let files = state.add_demo_server("files", stdio("files"), Status::Off);
        let id = state.servers[0].record.id.clone();
        let kept = state.servers[files].record.id.clone();
        state.push_history(CallRecord {
            id: "c1".into(),
            server_id: id.clone(),
            kind: mcp_store::CallKind::Tool,
            name: "get_weather".into(),
            args: json!({}),
            result: None,
            status: mcp_store::CallStatus::Ok,
            error: None,
            elapsed_ms: 1,
            at: time::OffsetDateTime::UNIX_EPOCH,
        });
        let key = |server: &str| (server.to_owned(), Mode::Tools, "get_weather".to_owned());
        for server in [&id, &kept] {
            state.responses.begin(key(server), "tools/call").unwrap();
        }
        state.expanded_log = [3].into();

        let removed = state.finish_delete(&id, Some(Ok(true)), None);
        assert_eq!(
            removed,
            Deletion::Removed(Gone::Server {
                id: id.clone(),
                calls: vec!["c1".to_owned()],
            })
        );
        assert!(state.responses.get(&key(&id)).is_none());
        assert!(!state.responses.is_pending(&key(&id)));
        assert!(
            state.responses.is_pending(&key(&kept)),
            "another server keeps its own"
        );
        assert!(
            state.expanded_log.is_empty(),
            "the row was the deleted server's"
        );
    }

    fn stdio(command: &str) -> ServerSpec {
        ServerSpec::Stdio {
            command: command.into(),
            args: vec![],
            env: Default::default(),
            cwd: None,
        }
    }

    fn bearer(keyring_id: &str) -> ServerSpec {
        ServerSpec::Http {
            url: "https://remote.test/mcp".into(),
            headers: Default::default(),
            auth: mcp_core::AuthRef::Bearer {
                keyring_id: keyring_id.into(),
            },
        }
    }

    /// A keyring that holds nothing and refuses every change.
    struct Refusing;

    impl mcp_auth::SecretStore for Refusing {
        fn get(&self, _: &str) -> Result<Option<String>, String> {
            Ok(None)
        }

        fn set(&self, _: &str, _: &str) -> Result<(), String> {
            Err("keychain locked".into())
        }

        fn delete(&self, _: &str) -> Result<(), String> {
            Err("keychain locked".into())
        }
    }

    // Model-only checks: `add_server`, `update_server` and `import_servers`
    // run the functions below in the background and apply the outcome with
    // `push_saved`, `apply_edit` and `apply_import`; the headless UI flows
    // drive them through the form.

    #[test]
    fn an_unavailable_database_refuses_to_add() {
        let reason = "cannot open /nowhere/coco.db: denied";
        let state = AppState::without_database(None, reason.into());
        let refused = state.writable_store().err().unwrap();
        assert!(refused.contains("denied"), "{refused}");
        assert!(state.servers.is_empty(), "nothing looks saved");
        assert_eq!(
            state.persistence.note().as_deref(),
            Some("not saving · cannot open /nowhere/coco.db: denied")
        );
    }

    #[test]
    fn the_server_view_always_offers_its_settings() {
        let mut state = AppState::new(None, None);
        state.mode = Mode::Server;
        assert!(state.items().is_empty(), "no server, no sections");
        state.add_demo_server("weather", stdio("weather"), Status::Off);
        let names = |state: &AppState| {
            state
                .items()
                .iter()
                .map(|item| item.name.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(&state), [SETTINGS_SECTION], "never connected");
        let snapshot: Snapshot = serde_json::from_value(json!({
            "protocolVersion": "2025-11-25",
            "serverInfo": {"name": "weather", "version": "1"},
            "capabilities": {},
            "takenAt": "2026-09-15T12:00:00Z"
        }))
        .unwrap();
        state.servers[0].set_snapshot(Some(snapshot));
        let names = names(&state);
        assert_eq!(names.first().map(String::as_str), Some("server"));
        assert_eq!(names.last().map(String::as_str), Some(SETTINGS_SECTION));
    }

    #[test]
    fn a_demo_model_adds_in_memory() {
        let mut state = AppState::new(None, None);
        assert!(state.writable_store().unwrap().is_none());
        let saved = persistence::insert_server(
            None,
            &*state.secrets,
            "weather",
            stdio("weather"),
            ProtocolMode::Auto,
            None,
        );
        assert_eq!(state.push_saved(saved.unwrap()), 0);
        assert_eq!(state.servers[0].record.protocol, ProtocolMode::Auto);
        assert_eq!(state.persistence, Persistence::Memory);
        assert_eq!(state.persistence.note(), None);
    }

    #[test]
    fn a_database_model_saves_before_it_shows() {
        let store = Store::open_in_memory().unwrap();
        let mut state = AppState::new(None, Some(store.clone()));
        assert_eq!(state.persistence, Persistence::Saved);
        let target = state.writable_store().unwrap();
        let saved = persistence::insert_server(
            target.as_ref(),
            &*state.secrets,
            "weather",
            stdio("weather"),
            ProtocolMode::Modern,
            None,
        );
        state.push_saved(saved.unwrap());
        assert_eq!(
            store.list_servers().unwrap()[0].protocol,
            ProtocolMode::Modern
        );
        assert_eq!(store.list_servers().unwrap().len(), 1);
        // Names are unique: the refusal goes back to the form, not the sidebar.
        let again = persistence::insert_server(
            target.as_ref(),
            &*state.secrets,
            "weather",
            stdio("other"),
            ProtocolMode::Legacy,
            None,
        );
        assert!(again.is_err());
        assert_eq!(state.servers.len(), 1);
        assert_eq!(
            state.persistence,
            Persistence::Saved,
            "a refused name is not a lost write"
        );
    }

    #[test]
    fn a_token_the_keyring_would_not_remove_is_reported() {
        let store = Store::open_in_memory().unwrap();
        let mut state = AppState::new(None, Some(store.clone()));
        let saved = persistence::insert_server(
            Some(&store),
            &mcp_auth::MemoryStore::new(),
            "remote",
            bearer("old"),
            ProtocolMode::Legacy,
            Some("s3cret"),
        );
        state.push_saved(saved.unwrap());
        let mut record = state.servers[0].record.clone();
        record.spec = stdio("local");
        let saved =
            persistence::replace_server(Some(&store), &Refusing, record, &bearer("old"), None);
        assert_eq!(state.apply_edit(saved.unwrap()), Some(0));
        assert_eq!(
            state.servers[0].record.spec,
            stdio("local"),
            "the row saved"
        );
        assert_eq!(store.list_servers().unwrap()[0].spec, stdio("local"));
        assert_eq!(
            state.persistence,
            Persistence::Failed(
                "old token of `remote` was not removed from the keyring: keychain locked".into()
            )
        );
    }

    #[test]
    fn an_imported_token_the_keyring_refused_is_reported() {
        let store = Store::open_in_memory().unwrap();
        let mut state = AppState::new(None, Some(store.clone()));
        let saved = persistence::insert_server(
            Some(&store),
            &Refusing,
            "paid",
            bearer("paid-key"),
            ProtocolMode::Legacy,
            Some("from-the-file"),
        );
        let mut result = Imported::default();
        let imported = ImportedServer {
            name: "paid".into(),
            needs_token: false,
            saved,
        };
        assert_eq!(state.apply_import(&mut result, vec![imported]), Some(0));
        assert_eq!(result.summary(), "imported 1 server · 1 need a token");
        assert_eq!(
            state.persistence,
            Persistence::Failed(
                "token of `paid` was not stored in the keyring: keychain locked".into()
            )
        );
    }

    #[test]
    fn an_import_the_database_could_not_write_is_reported() {
        let store = Store::open_in_memory().unwrap();
        let mut state = AppState::new(None, Some(store.clone()));
        let secrets = mcp_auth::MemoryStore::new();
        let import = |name: &str, spec: ServerSpec| ImportedServer {
            name: name.into(),
            needs_token: false,
            saved: persistence::insert_server(
                Some(&store),
                &secrets,
                name,
                spec,
                ProtocolMode::Legacy,
                None,
            ),
        };
        let notes = import("notes", stdio("notes"));
        let mut env = std::collections::BTreeMap::new();
        env.insert("GITHUB_TOKEN".to_owned(), "ghp_x".to_owned());
        let github = import(
            "github",
            ServerSpec::Stdio {
                command: "gh-mcp".into(),
                args: vec![],
                env,
                cwd: None,
            },
        );
        assert!(
            matches!(github.saved, Err(Unsaved::Refused(_))),
            "{github:?}"
        );
        let lost = || ImportedServer {
            name: "weather".into(),
            needs_token: false,
            saved: Err(Unsaved::Failed("disk I/O error".into())),
        };

        let mut result = Imported::default();
        let first = state.apply_import(&mut result, vec![notes, github, lost()]);
        assert_eq!(first, Some(0));
        assert_eq!(
            result.summary(),
            "imported 1 server · 1 skipped · 1 not saved"
        );
        assert_eq!(state.servers.len(), 1);
        assert_eq!(
            state.persistence,
            Persistence::Failed("`weather` was not imported: disk I/O error".into()),
            "a refused secret is a skipped entry, a failed write is shown"
        );

        let mut state = AppState::without_database(None, "denied".into());
        let mut result = Imported::default();
        assert_eq!(state.apply_import(&mut result, vec![lost()]), None);
        assert_eq!(result.not_saved, 1);
        assert_eq!(state.persistence, Persistence::Unavailable("denied".into()));
    }

    #[test]
    fn a_failed_delete_keeps_the_server() {
        let mut state = demo();
        let id = state.servers[0].record.id.clone();
        assert_eq!(
            state.finish_delete(&id, Some(Err("disk I/O error".into())), None),
            Deletion::Kept
        );
        assert_eq!(state.servers.len(), 1, "the server stays in the sidebar");
        assert_eq!(state.selected_server, Some(0));
        assert_eq!(
            state.persistence,
            Persistence::Failed("`weather` was not deleted: disk I/O error".into())
        );
    }

    #[test]
    fn a_failed_keyring_delete_is_reported() {
        let mut state = demo();
        let id = state.servers[0].record.id.clone();
        assert!(matches!(
            state.finish_delete(&id, Some(Ok(true)), Some(Err("locked".into()))),
            Deletion::Removed(_)
        ));
        assert!(
            state.servers.is_empty(),
            "the row is gone, so is the server"
        );
        assert_eq!(state.selected_server, None);
        let note = state.persistence.note().unwrap();
        assert!(
            note.contains("keyring") && note.contains("locked"),
            "{note}"
        );
    }

    #[test]
    fn a_background_delete_keeps_indices_on_their_servers() {
        let mut state = demo();
        let files = state.add_demo_server("files", stdio("files"), Status::Off);
        state.selected_server = Some(files);
        state.selected_item = Some(0);
        state.editing = Some(files);
        state.screen = Screen::AddServer;
        let id = state.servers[0].record.id.clone();
        assert!(matches!(
            state.finish_delete(&id, Some(Ok(true)), None),
            Deletion::Removed(_)
        ));
        assert_eq!(state.server().unwrap().record.name, "files");
        assert_eq!(state.selected_item, Some(0), "another server was deleted");
        assert_eq!(
            state.editing, None,
            "the form cannot save over a shifted row"
        );
        assert_eq!(state.screen, Screen::Detail);
        assert_eq!(state.persistence, Persistence::Memory);
        assert_eq!(
            state.finish_delete(&id, Some(Ok(false)), None),
            Deletion::Unknown,
            "a second delete of the same server finds nothing"
        );
    }

    #[test]
    fn note_write_is_sticky() {
        let mut state = AppState::new(None, Some(Store::open_in_memory().unwrap()));
        assert_eq!(
            state.note_write("theme was not saved", Ok::<_, String>(1)),
            Some(1)
        );
        assert_eq!(state.persistence, Persistence::Saved);
        assert_eq!(
            state.note_write("theme was not saved", Err::<(), _>("locked")),
            None
        );
        assert_eq!(
            state.note_write("theme was not saved", Ok::<_, String>(2)),
            Some(2)
        );
        assert_eq!(
            state.persistence,
            Persistence::Failed("theme was not saved: locked".into()),
            "a later success does not clear a failure"
        );
        state.note_failure("call to `add` was not recorded", "disk full");
        assert_eq!(
            state.persistence.note().as_deref(),
            Some("call to `add` was not recorded: disk full"),
            "the last failure is named"
        );

        let mut state = AppState::without_database(None, "denied".into());
        state.note_failure(
            "stored token of `x` was not removed from the keyring",
            "locked",
        );
        assert_eq!(
            state.persistence,
            Persistence::Unavailable("denied".into()),
            "nothing is saved at all, which says more"
        );
    }

    #[test]
    fn a_connect_authorizes_from_the_challenge_the_server_sent() {
        let mut state = AppState::new(None, None);
        let spec = ServerSpec::Http {
            url: "https://remote.test/mcp".into(),
            headers: Default::default(),
            auth: mcp_core::AuthRef::OAuth {
                keyring_id: "remote".into(),
            },
        };
        let ix = state.add_demo_server("remote", spec, Status::Off);
        assert_eq!(state.oauth_options(ix).challenge, None);
        let challenge = r#"Bearer resource_metadata="https://remote.test/meta""#;
        state.servers[ix].auth_challenge = Some(challenge.into());
        assert_eq!(
            state.oauth_options(ix).challenge.as_deref(),
            Some(challenge)
        );
    }

    #[test]
    fn a_separator_that_pushes_rows_out_names_them() {
        let mut state = AppState::new(None, None);
        let ix = state.add_demo_server("s", stdio("s"), Status::Off);
        for i in 0..MAX_LOG_ROWS {
            state.servers[ix].push_log(stderr_row(&format!("line {i}")));
        }
        let id = state.servers[ix].record.id.clone();
        assert_eq!(
            state.open_session_log(ix),
            Some(Gone::LogRows {
                server_id: id,
                before: 1
            })
        );
        let log = state.servers[ix].log();
        assert_eq!(log.len(), MAX_LOG_ROWS);
        assert!(log.last().is_some_and(|row| row.session_break));
        assert_eq!(
            state.open_session_log(ix),
            None,
            "a session that logged nothing adds no second separator"
        );
    }

    #[test]
    fn a_confirmation_follows_its_server_when_another_is_deleted() {
        let mut state = AppState::new(None, None);
        for name in ["a", "b", "c"] {
            state.add_demo_server(name, stdio(name), Status::Off);
        }
        let first = state.servers[0].record.id.clone();
        state.confirm = Some(Confirm::ClearHistory(2));
        assert!(matches!(
            state.finish_delete(&first, None, None),
            Deletion::Removed(_)
        ));
        assert_eq!(state.confirm, Some(Confirm::ClearHistory(1)), "followed");
        let confirmed = state.servers[1].record.id.clone();
        assert!(matches!(
            state.finish_delete(&confirmed, None, None),
            Deletion::Removed(_)
        ));
        assert_eq!(state.confirm, None, "its server is gone");
    }

    #[test]
    fn only_a_server_with_credentials_is_asked_about_them() {
        let mut state = AppState::new(None, None);
        state.add_demo_server("plain", stdio("plain"), Status::Off);
        assert!(keyring_id(&state.servers[0].record.spec).is_none());
        let bearer = ServerSpec::Http {
            url: "https://remote.test/mcp".into(),
            headers: Default::default(),
            auth: mcp_core::AuthRef::Bearer {
                keyring_id: "k".into(),
            },
        };
        state.add_demo_server("remote", bearer, Status::Off);
        assert_eq!(
            keyring_id(&state.servers[1].record.spec).as_deref(),
            Some("k")
        );
        assert_eq!(Confirm::ForgetCredentials(1).server(), 1);
        assert_eq!(
            Confirm::Delete(3).for_server(2),
            Confirm::Delete(2),
            "an index moves, the action does not"
        );
    }

    #[test]
    fn history_rows_date_calls_from_another_day() {
        use time::macros::datetime;
        let now = datetime!(2026-09-13 10:00 UTC);
        assert_eq!(
            listed_label(datetime!(2026-09-13 09:05:01.250 UTC), now),
            "09:05:01.250"
        );
        assert_eq!(
            listed_label(datetime!(2026-09-12 22:41:02 UTC), now),
            "Sep 12 22:41"
        );
        assert_eq!(
            listed_label(datetime!(2025-12-31 23:59 UTC), now),
            "2025-12-31 23:59"
        );
    }
}
