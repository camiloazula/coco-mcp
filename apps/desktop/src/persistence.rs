//! Whether what the user changes reaches the database and the keyring.
//!
//! A failed write is never only a log line: the outcome lives on the model as
//! [`Persistence`] and the status bar shows it, so a server that looks saved
//! is saved.

use std::collections::HashSet;
use std::fmt::Display;

use futures::channel::oneshot;
use gpui_kit::Context;
use mcp_auth::SecretStore;
use mcp_core::{ServerRequestPolicy, ServerSpec, Snapshot};
use mcp_store::{CallRecord, ServerRecord, Store};

use crate::state::{AppState, MAX_HISTORY, demo_record, keyring_id};

/// Resolves once a server has been saved, or with the reason it was not.
pub type Saving = oneshot::Receiver<Result<(), String>>;

/// A [`Saving`] that is already refused.
pub(crate) fn refused(reason: String) -> Saving {
    let (done, saving) = oneshot::channel();
    let _ = done.send(Err(reason));
    saving
}

/// Where the model's changes go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Persistence {
    /// Every write so far reached the database.
    Saved,
    /// No database on purpose (demo data, tests). Nothing to report.
    Memory,
    /// The app wanted a database and could not open one. Nothing is saved
    /// this session, so adding, editing and importing servers are refused.
    Unavailable(String),
    /// The database is open but a write failed. Kept for the session and
    /// naming the last failure; later writes still try.
    Failed(String),
}

impl Persistence {
    /// Status bar text, when there is something to report.
    pub fn note(&self) -> Option<String> {
        match self {
            Self::Unavailable(reason) => Some(format!("not saving · {reason}")),
            Self::Failed(reason) => Some(reason.clone()),
            Self::Saved | Self::Memory => None,
        }
    }
}

impl AppState {
    /// Pass `result` through, remembering a failure as [`Persistence::Failed`].
    /// `what` says what was lost, e.g. "call to `add` was not recorded".
    pub(crate) fn note_write<T, E: Display>(
        &mut self,
        what: &str,
        result: Result<T, E>,
    ) -> Option<T> {
        match result {
            Ok(value) => Some(value),
            Err(e) => {
                self.note_failure(what, e);
                None
            }
        }
    }

    /// Remember a failed write. An unavailable database stays the headline:
    /// it already says that nothing is saved.
    pub(crate) fn note_failure(&mut self, what: &str, error: impl Display) {
        tracing::warn!("{what}: {error}");
        if !matches!(self.persistence, Persistence::Unavailable(_)) {
            self.persistence = Persistence::Failed(format!("{what}: {error}"));
        }
    }

    /// The store a change must be written to before the model shows it.
    /// `Ok(None)` for a model kept in memory on purpose; an error when the
    /// database could not be opened, so nothing pretends to be saved.
    pub(crate) fn writable_store(&self) -> Result<Option<Store>, String> {
        match &self.persistence {
            Persistence::Unavailable(reason) => Err(format!("not saved: {reason}")),
            Persistence::Memory => Ok(None),
            Persistence::Saved | Persistence::Failed(_) => Ok(self.store.clone()),
        }
    }

    /// Run blocking database or keyring `work` on the runtime's blocking
    /// pool, then hand its result to `done` on the model. A model without a
    /// bridge (demos, tests) runs both in place. `done` receives `None` when
    /// the work panicked.
    pub(crate) fn in_background<R: Send + 'static>(
        &mut self,
        work: impl FnOnce() -> R + Send + 'static,
        done: impl FnOnce(&mut Self, Option<R>, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        match self.bridge.clone() {
            Some(bridge) => {
                let task = bridge.run_blocking(work);
                cx.spawn(async move |this, cx| {
                    let result = task.await;
                    let _ = this.update(cx, |state, cx| done(state, result, cx));
                })
                .detach();
            }
            None => done(self, Some(work()), cx),
        }
    }
}

/// Compare `snapshot` with the last one stored for `server_id` and store it
/// when its content changed, so reconnects do not pile up identical rows. A
/// snapshot with a list that failed is compared but not stored:
/// [`Store::add_snapshot`] refuses it for the app and `coco --db` alike.
/// Blocking.
///
/// Returns the diff (`None` when the stored one could not be read) and the
/// text of the first error.
pub fn compare_and_store(
    store: &Store,
    server_id: &str,
    snapshot: &Snapshot,
) -> (Option<mcp_diff::SnapshotDiff>, Option<String>) {
    let previous = match store.latest_snapshot(server_id) {
        Ok(previous) => previous.map(|r| r.snapshot),
        Err(e) => return (None, Some(e.to_string())),
    };
    let diff = mcp_diff::diff(previous.as_ref(), snapshot);
    // Decided by content rather than by the diff, which skips a list that
    // failed on either side and so can say unchanged when the digests differ.
    let changed = previous
        .as_ref()
        .is_none_or(|p| p.digest() != snapshot.digest());
    let error = if changed {
        store.add_snapshot(server_id, snapshot).err()
    } else {
        None
    };
    (Some(diff), error.map(|e| e.to_string()))
}

/// Each call with what its result lays out, measured where the calls were
/// read, on the blocking pool, since a result may run to megabytes.
pub fn measured(calls: Vec<CallRecord>) -> Vec<(CallRecord, usize)> {
    calls
        .into_iter()
        .map(|call| {
            let size = crate::views::stored_size(&call);
            (call, size)
        })
        .collect()
}

/// A server's recorded calls once its stored history is read: the calls
/// recorded this session while it was being read, and the stored ones, each
/// once, newest first as the store lists them, at most [`MAX_HISTORY`].
pub fn merge_history(session: Vec<CallRecord>, stored: Vec<CallRecord>) -> Vec<CallRecord> {
    let mut seen = HashSet::new();
    let mut calls: Vec<_> = session
        .into_iter()
        .chain(stored)
        .filter(|call| seen.insert(call.id.clone()))
        .collect();
    calls.sort_by(|a, b| (b.at, &b.id).cmp(&(a.at, &a.id)));
    calls.truncate(MAX_HISTORY);
    calls
}

/// Why a server was not saved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unsaved {
    /// The database turned the server down as described: its name is taken
    /// or its spec holds a secret. Nothing was lost.
    Refused(String),
    /// The write failed, or there is no database to write to.
    Failed(String),
}

impl Display for Unsaved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(reason) | Self::Failed(reason) => f.write_str(reason),
        }
    }
}

impl From<mcp_store::Error> for Unsaved {
    fn from(error: mcp_store::Error) -> Self {
        if error.is_refusal() {
            Self::Refused(error.to_string())
        } else {
            Self::Failed(error.to_string())
        }
    }
}

/// A server row the database accepted, and what around it the keyring did
/// not: each entry says what was lost and why. The row stays saved.
#[derive(Debug)]
pub struct SavedServer {
    /// The row as saved.
    pub record: ServerRecord,
    /// Keyring writes that failed, for `AppState::note_failure`.
    pub lost: Vec<(String, String)>,
}

/// Save a new server, connected in `protocol`, then put `token` in the
/// keyring entry its spec names. `store` is `None` for a model kept in memory
/// on purpose. Blocking.
///
/// The token goes in only once the row exists, so a server the database
/// refuses (a name already taken) leaves nothing behind in the keyring.
pub fn insert_server(
    store: Option<&Store>,
    secrets: &dyn SecretStore,
    name: &str,
    spec: ServerSpec,
    protocol: mcp_core::ProtocolMode,
    token: Option<&str>,
) -> Result<SavedServer, Unsaved> {
    let record = match store {
        Some(store) => {
            store.add_server_with(name, &spec, &ServerRequestPolicy::default(), protocol)?
        }
        None => ServerRecord {
            protocol,
            ..demo_record(name, spec)
        },
    };
    let lost = store_token(secrets, &record, token).into_iter().collect();
    Ok(SavedServer { record, lost })
}

/// Save `record` over its row, then write `token` and remove the keyring
/// entry `previous` named when the new spec no longer uses it. Blocking.
///
/// Both keyring writes wait for the row, so an edit the database refuses
/// leaves the server with the token it had.
pub fn replace_server(
    store: Option<&Store>,
    secrets: &dyn SecretStore,
    record: ServerRecord,
    previous: &ServerSpec,
    token: Option<&str>,
) -> Result<SavedServer, String> {
    if let Some(store) = store {
        store.update_server(&record).map_err(|e| e.to_string())?;
    }
    let mut lost: Vec<_> = store_token(secrets, &record, token).into_iter().collect();
    let current = keyring_id(&record.spec);
    if let Some(unused) = keyring_id(previous).filter(|old| current.as_ref() != Some(old))
        && let Err(e) = secrets.delete(&unused)
    {
        let what = format!(
            "old token of `{}` was not removed from the keyring",
            record.name
        );
        lost.push((what, e));
    }
    Ok(SavedServer { record, lost })
}

/// Write `token` to the keyring entry `record` names. Returns what was lost
/// when the keyring refused it.
fn store_token(
    secrets: &dyn SecretStore,
    record: &ServerRecord,
    token: Option<&str>,
) -> Option<(String, String)> {
    let key = keyring_id(&record.spec)?;
    let error = secrets.set(&key, token?).err()?;
    let what = format!("token of `{}` was not stored in the keyring", record.name);
    Some((what, error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mcp_core::{ServerRequestPolicy, ServerSpec};
    use serde_json::json;

    fn snapshot(tools: &[&str]) -> Snapshot {
        let tools: Vec<_> = tools
            .iter()
            .map(|name| json!({"name": name, "inputSchema": {"type": "object"}}))
            .collect();
        serde_json::from_value(json!({
            "protocolVersion": "2025-11-25",
            "serverInfo": {"name": "weather", "version": "0.4.1"},
            "capabilities": {"tools": {}},
            "tools": tools,
            "takenAt": "2026-09-11T12:00:00Z"
        }))
        .unwrap()
    }

    #[test]
    fn notes_say_what_is_not_saved() {
        assert_eq!(Persistence::Saved.note(), None);
        assert_eq!(Persistence::Memory.note(), None);
        assert_eq!(
            Persistence::Unavailable("cannot open /x/coco.db: denied".into()).note(),
            Some("not saving · cannot open /x/coco.db: denied".into())
        );
        assert_eq!(
            Persistence::Failed("`weather` was not deleted: disk I/O error".into()).note(),
            Some("`weather` was not deleted: disk I/O error".into())
        );
    }

    #[test]
    fn a_changed_snapshot_is_stored_once() {
        let store = Store::open_in_memory().unwrap();
        let spec = ServerSpec::Stdio {
            command: "weather".into(),
            args: vec![],
            env: Default::default(),
            cwd: None,
        };
        let record = store
            .add_server("weather", &spec, &ServerRequestPolicy::default())
            .unwrap();
        let first = snapshot(&["get_weather"]);

        let (diff, error) = compare_and_store(&store, &record.id, &first);
        assert_eq!(error, None);
        assert_eq!(diff.unwrap().outcome, mcp_diff::Outcome::First);
        let (diff, error) = compare_and_store(&store, &record.id, &first);
        assert_eq!(error, None);
        assert_eq!(diff.unwrap().outcome, mcp_diff::Outcome::Unchanged);
        assert_eq!(store.list_snapshots(&record.id, 10).unwrap().len(), 1);

        let (diff, error) = compare_and_store(&store, &record.id, &snapshot(&[]));
        assert_eq!(error, None);
        assert!(diff.unwrap().has_breaking());
        assert_eq!(store.list_snapshots(&record.id, 10).unwrap().len(), 2);
    }

    #[test]
    fn a_partial_snapshot_is_compared_but_not_stored() {
        let store = Store::open_in_memory().unwrap();
        let spec = ServerSpec::Stdio {
            command: "weather".into(),
            args: vec![],
            env: Default::default(),
            cwd: None,
        };
        let record = store
            .add_server("weather", &spec, &ServerRequestPolicy::default())
            .unwrap();
        let mut partial = snapshot(&[]);
        partial.list_failures.push(mcp_core::ListFailure {
            method: "tools/list".into(),
            error: "request timed out after 60s".into(),
        });

        let (diff, error) = compare_and_store(&store, &record.id, &partial);
        assert_eq!(error, None);
        assert_eq!(diff.unwrap().outcome, mcp_diff::Outcome::First);
        assert!(store.list_snapshots(&record.id, 10).unwrap().is_empty());

        // Against a stored baseline, the unread tools are not reported removed.
        let (_, error) = compare_and_store(&store, &record.id, &snapshot(&["get_weather"]));
        assert_eq!(error, None);
        let (diff, error) = compare_and_store(&store, &record.id, &partial);
        assert_eq!(error, None);
        let diff = diff.unwrap();
        assert!(!diff.has_breaking());
        assert_eq!(diff.skipped, ["tools/list"]);
        assert_eq!(store.list_snapshots(&record.id, 10).unwrap().len(), 1);
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

    #[test]
    fn a_server_the_database_refuses_leaves_the_keyring_as_it_was() {
        let store = Store::open_in_memory().unwrap();
        let secrets = mcp_auth::MemoryStore::new();
        let weather = ServerSpec::Stdio {
            command: "weather".into(),
            args: vec![],
            env: Default::default(),
            cwd: None,
        };
        store
            .add_server("weather", &weather, &ServerRequestPolicy::default())
            .unwrap();
        let remote = insert_server(
            Some(&store),
            &secrets,
            "remote",
            bearer("remote-key"),
            mcp_core::ProtocolMode::Legacy,
            Some("old"),
        )
        .unwrap();
        assert!(remote.lost.is_empty());

        // A new server under a name already taken leaves no token behind.
        let refused = insert_server(
            Some(&store),
            &secrets,
            "weather",
            bearer("fresh-key"),
            mcp_core::ProtocolMode::Legacy,
            Some("new"),
        );
        assert!(matches!(refused, Err(Unsaved::Refused(_))), "{refused:?}");
        assert_eq!(secrets.get("fresh-key").unwrap(), None);

        // Renamed to a name already taken, with a new token typed in.
        let mut record = remote.record.clone();
        record.name = "weather".into();
        let refused = replace_server(
            Some(&store),
            &secrets,
            record,
            &bearer("remote-key"),
            Some("new"),
        );
        assert!(refused.is_err());
        assert_eq!(
            secrets.get("remote-key").unwrap().as_deref(),
            Some("old"),
            "the server keeps the token it had"
        );
        assert_eq!(secrets.len(), 1);
        let stored = store.get_server(&remote.record.id).unwrap().unwrap();
        assert_eq!(stored.name, "remote");
    }

    #[test]
    fn an_edit_removes_the_token_it_no_longer_uses() {
        let store = Store::open_in_memory().unwrap();
        let secrets = mcp_auth::MemoryStore::new();
        let saved = insert_server(
            Some(&store),
            &secrets,
            "remote",
            bearer("old"),
            mcp_core::ProtocolMode::Legacy,
            Some("s3cret"),
        )
        .unwrap();
        let kept =
            replace_server(Some(&store), &secrets, saved.record, &bearer("old"), None).unwrap();
        assert_eq!(
            secrets.get("old").unwrap().as_deref(),
            Some("s3cret"),
            "still in use"
        );

        let mut record = kept.record;
        record.name = "local".into();
        record.spec = ServerSpec::Stdio {
            command: "local".into(),
            args: vec![],
            env: Default::default(),
            cwd: None,
        };
        let moved = replace_server(Some(&store), &secrets, record, &bearer("old"), None).unwrap();
        assert!(moved.lost.is_empty());
        assert!(secrets.is_empty());
        assert_eq!(store.list_servers().unwrap()[0].name, "local");
    }

    fn call(id: &str, second: i64) -> CallRecord {
        CallRecord {
            id: id.into(),
            server_id: "weather".into(),
            kind: mcp_store::CallKind::Tool,
            name: id.into(),
            args: json!({}),
            result: None,
            status: mcp_store::CallStatus::Ok,
            error: None,
            elapsed_ms: 1,
            at: time::OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(second),
        }
    }

    fn ids(calls: &[CallRecord]) -> Vec<&str> {
        calls.iter().map(|c| c.id.as_str()).collect()
    }

    #[test]
    fn history_read_late_keeps_the_calls_of_the_session_once() {
        // `b` was recorded this session and is already in the store.
        let session = vec![call("d", 4), call("b", 2)];
        let stored = vec![call("c", 3), call("b", 2), call("a", 1)];
        assert_eq!(ids(&merge_history(session, stored)), ["d", "c", "b", "a"]);

        // Calls made in the same instant are ordered by id, as the store does.
        let tied = merge_history(vec![call("x", 5)], vec![call("y", 5), call("w", 5)]);
        assert_eq!(ids(&tied), ["y", "x", "w"]);

        assert_eq!(ids(&merge_history(vec![call("only", 9)], vec![])), ["only"]);

        let stored: Vec<_> = (0..MAX_HISTORY as i64)
            .map(|i| call(&format!("s{i:04}"), i))
            .rev()
            .collect();
        let merged = merge_history(vec![call("new", MAX_HISTORY as i64)], stored);
        assert_eq!(merged.len(), MAX_HISTORY);
        assert_eq!(merged[0].id, "new");
        assert_eq!(merged[MAX_HISTORY - 1].id, "s0001", "the oldest goes");
    }

    #[test]
    fn a_snapshot_that_cannot_be_stored_says_why() {
        let store = Store::open_in_memory().unwrap();
        // No server row: the foreign key refuses the insert.
        let (diff, error) = compare_and_store(&store, "gone", &snapshot(&["get_weather"]));
        assert_eq!(diff.unwrap().outcome, mcp_diff::Outcome::First);
        assert!(error.unwrap().contains("FOREIGN KEY"));
    }
}
