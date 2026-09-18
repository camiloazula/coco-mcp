//! The [`Store`] handle and its queries.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use mcp_core::{ProtocolMode, ServerRequestPolicy, ServerSpec, Snapshot};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::migrations::MIGRATIONS;
use crate::records::{
    CallKind, CallRecord, CallStatus, NewCall, ServerRecord, SnapshotRecord, SnapshotSummary,
};
use crate::{Error, Result};

/// Make `path` readable and writable by its owner alone. Best effort: a
/// filesystem that has no modes, or refuses, leaves the default.
#[cfg(unix)]
fn owner_only(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

/// Windows has no mode bits; the profile directory is the user's own.
#[cfg(not(unix))]
fn owner_only(_path: &Path) {}

/// Cheap-to-clone handle to one SQLite database.
#[derive(Debug, Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
}

fn now() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".into())
}

fn parse_time(s: &str) -> Result<OffsetDateTime> {
    OffsetDateTime::parse(s, &Rfc3339).map_err(|_| Error::Timestamp(s.to_owned()))
}

fn new_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

impl Store {
    /// Open (creating if needed) the database at `path` and migrate it.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        // Call arguments and results are the user's alone. Done before the
        // first write: SQLite gives the WAL and shared-memory files the
        // database file's mode when it creates them.
        owner_only(path);
        Self::init(conn)
    }

    /// A private in-memory database (tests, `coco` without `--db`).
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    /// `coco.db` in the platform's data directory for
    /// `coco-mcp`.
    pub fn default_path() -> Option<PathBuf> {
        directories::ProjectDirs::from("", "", "coco-mcp")
            .map(|dirs| dirs.data_dir().join("coco.db"))
    }

    fn init(mut conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        MIGRATIONS.to_latest(&mut conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn conn(&self) -> Result<MutexGuard<'_, Connection>> {
        self.conn.lock().map_err(|_| Error::Poisoned)
    }

    // ------------------------------------------------------------------ servers

    /// Reject specs that would persist a secret in plain text.
    fn check_spec(spec: &ServerSpec) -> Result<()> {
        if let ServerSpec::Http { headers, .. } = spec {
            for key in headers.keys() {
                let lower = key.to_ascii_lowercase();
                if lower == "authorization" || lower == "proxy-authorization" {
                    return Err(Error::SecretInSpec(format!(
                        "header `{key}` must be configured through auth, not stored as a header"
                    )));
                }
            }
        }
        if let ServerSpec::Stdio { env, .. } = spec {
            for key in env.keys() {
                let upper = key.to_ascii_uppercase();
                if upper.ends_with("_TOKEN")
                    || upper.ends_with("_SECRET")
                    || upper.ends_with("_API_KEY")
                    || upper == "API_KEY"
                {
                    return Err(Error::SecretInSpec(format!(
                        "environment variable `{key}` looks like a secret; pass it through the keyring"
                    )));
                }
            }
        }
        Ok(())
    }

    /// Save a new server, connected with the handshake
    /// ([`ProtocolMode::Legacy`]). `name` must be unique.
    pub fn add_server(
        &self,
        name: &str,
        spec: &ServerSpec,
        policy: &ServerRequestPolicy,
    ) -> Result<ServerRecord> {
        self.add_server_with(name, spec, policy, ProtocolMode::Legacy)
    }

    /// [`Self::add_server`], connected in `protocol`.
    pub fn add_server_with(
        &self,
        name: &str,
        spec: &ServerSpec,
        policy: &ServerRequestPolicy,
        protocol: ProtocolMode,
    ) -> Result<ServerRecord> {
        Self::check_spec(spec)?;
        let id = new_id();
        self.conn()?.execute(
            "INSERT INTO servers (id, name, spec, policy, protocol) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                id,
                name,
                serde_json::to_string(spec)?,
                serde_json::to_string(policy)?,
                protocol.as_str()
            ],
        )?;
        self.get_server(&id)?
            .ok_or_else(|| Error::NotFound(format!("server {id}")))
    }

    /// Update name, spec, policy and protocol of an existing server.
    pub fn update_server(&self, record: &ServerRecord) -> Result<()> {
        Self::check_spec(&record.spec)?;
        let changed = self.conn()?.execute(
            "UPDATE servers SET name = ?2, spec = ?3, policy = ?4, protocol = ?5 WHERE id = ?1",
            params![
                record.id,
                record.name,
                serde_json::to_string(&record.spec)?,
                serde_json::to_string(&record.policy)?,
                record.protocol.as_str()
            ],
        )?;
        if changed == 0 {
            return Err(Error::NotFound(format!("server {}", record.id)));
        }
        Ok(())
    }

    /// Fetch a server by id.
    pub fn get_server(&self, id: &str) -> Result<Option<ServerRecord>> {
        self.conn()?
            .query_row(
                "SELECT id, name, spec, policy, protocol FROM servers WHERE id = ?1",
                params![id],
                row_to_server,
            )
            .optional()?
            .transpose()
    }

    /// Fetch a server by its unique name.
    pub fn find_server_by_name(&self, name: &str) -> Result<Option<ServerRecord>> {
        self.conn()?
            .query_row(
                "SELECT id, name, spec, policy, protocol FROM servers WHERE name = ?1",
                params![name],
                row_to_server,
            )
            .optional()?
            .transpose()
    }

    /// Fetch the server named `name`, creating it when missing.
    pub fn get_or_add_server(&self, name: &str, spec: &ServerSpec) -> Result<ServerRecord> {
        match self.find_server_by_name(name)? {
            Some(existing) => Ok(existing),
            None => self.add_server(name, spec, &ServerRequestPolicy::default()),
        }
    }

    /// All servers, by name.
    pub fn list_servers(&self) -> Result<Vec<ServerRecord>> {
        let conn = self.conn()?;
        let mut stmt =
            conn.prepare("SELECT id, name, spec, policy, protocol FROM servers ORDER BY name")?;
        let rows = stmt.query_map([], row_to_server)?;
        rows.map(|r| r?).collect()
    }

    /// Delete a server and, by cascade, its snapshots and calls.
    pub fn delete_server(&self, id: &str) -> Result<bool> {
        Ok(self
            .conn()?
            .execute("DELETE FROM servers WHERE id = ?1", params![id])?
            > 0)
    }

    // ---------------------------------------------------------------- snapshots

    /// Store a snapshot for `server_id`. A snapshot with a list that failed
    /// ([`Snapshot::list_failures`]) is not stored and gives `Ok(None)`: it
    /// would become the baseline later snapshots and `saved:` diffs are
    /// measured against, with that list unread. The rule lives here so the
    /// app and `coco --db` cannot keep different baselines in one database.
    pub fn add_snapshot(
        &self,
        server_id: &str,
        snapshot: &Snapshot,
    ) -> Result<Option<SnapshotRecord>> {
        if !snapshot.list_failures.is_empty() {
            return Ok(None);
        }
        let id = new_id();
        let digest = snapshot.digest();
        let taken_at = snapshot
            .taken_at
            .format(&Rfc3339)
            .map_err(|_| Error::Timestamp("taken_at".into()))?;
        self.conn()?.execute(
            "INSERT INTO snapshots (id, server_id, digest, json, taken_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                id,
                server_id,
                digest,
                serde_json::to_string(snapshot)?,
                taken_at
            ],
        )?;
        Ok(Some(SnapshotRecord {
            id,
            server_id: server_id.to_owned(),
            digest,
            snapshot: snapshot.clone(),
        }))
    }

    /// The most recent snapshot of `server_id`.
    pub fn latest_snapshot(&self, server_id: &str) -> Result<Option<SnapshotRecord>> {
        self.conn()?
            .query_row(
                "SELECT id, server_id, digest, json FROM snapshots WHERE server_id = ?1 ORDER BY taken_at DESC, id DESC LIMIT 1",
                params![server_id],
                row_to_snapshot,
            )
            .optional()?
            .transpose()
    }

    /// Fetch a snapshot by id.
    pub fn get_snapshot(&self, id: &str) -> Result<Option<SnapshotRecord>> {
        self.conn()?
            .query_row(
                "SELECT id, server_id, digest, json FROM snapshots WHERE id = ?1",
                params![id],
                row_to_snapshot,
            )
            .optional()?
            .transpose()
    }

    /// Snapshot summaries of `server_id`, newest first.
    pub fn list_snapshots(&self, server_id: &str, limit: usize) -> Result<Vec<SnapshotSummary>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, server_id, digest, taken_at FROM snapshots WHERE server_id = ?1 ORDER BY taken_at DESC, id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![server_id, limit as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        rows.map(|r| {
            let (id, server_id, digest, taken_at) = r?;
            Ok(SnapshotSummary {
                id,
                server_id,
                digest,
                taken_at: parse_time(&taken_at)?,
            })
        })
        .collect()
    }

    // -------------------------------------------------------------------- calls

    /// Append a call to the history.
    pub fn record_call(&self, call: NewCall) -> Result<CallRecord> {
        let id = new_id();
        let at = now();
        self.conn()?.execute(
            "INSERT INTO calls (id, server_id, kind, name, args, result, status, error, elapsed_ms, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                id,
                call.server_id,
                call.kind.as_str(),
                call.name,
                serde_json::to_string(&call.args)?,
                call.result
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?,
                call.status.as_str(),
                call.error,
                call.elapsed_ms as i64,
                at
            ],
        )?;
        Ok(CallRecord {
            id,
            server_id: call.server_id,
            kind: call.kind,
            name: call.name,
            args: call.args,
            result: call.result,
            status: call.status,
            error: call.error,
            elapsed_ms: call.elapsed_ms,
            at: parse_time(&at)?,
        })
    }

    /// Calls, newest first, optionally restricted to one server.
    ///
    /// Ordered by the time `at` names, not its text: RFC 3339 drops trailing
    /// zeros from the fraction, so `…05.1Z` would sort after `…05.12Z`.
    /// Calls within the same millisecond fall back to their time-ordered ids.
    pub fn list_calls(&self, server_id: Option<&str>, limit: usize) -> Result<Vec<CallRecord>> {
        let conn = self.conn()?;
        let sql = match server_id {
            Some(_) => {
                "SELECT id, server_id, kind, name, args, result, status, error, elapsed_ms, at FROM calls WHERE server_id = ?1 ORDER BY julianday(at) DESC, id DESC LIMIT ?2"
            }
            None => {
                "SELECT id, server_id, kind, name, args, result, status, error, elapsed_ms, at FROM calls WHERE ?1 IS NULL ORDER BY julianday(at) DESC, id DESC LIMIT ?2"
            }
        };
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map(params![server_id, limit as i64], row_to_call)?;
        rows.map(|r| r?).collect()
    }

    /// Fetch one call.
    pub fn get_call(&self, id: &str) -> Result<Option<CallRecord>> {
        self.conn()?
            .query_row(
                "SELECT id, server_id, kind, name, args, result, status, error, elapsed_ms, at FROM calls WHERE id = ?1",
                params![id],
                row_to_call,
            )
            .optional()?
            .transpose()
    }

    /// Delete one recorded call. Returns whether a row was removed.
    pub fn delete_call(&self, id: &str) -> Result<bool> {
        let removed = self
            .conn()?
            .execute("DELETE FROM calls WHERE id = ?1", params![id])?;
        Ok(removed > 0)
    }

    /// Delete history, optionally for one server. Returns rows removed.
    pub fn clear_calls(&self, server_id: Option<&str>) -> Result<usize> {
        let conn = self.conn()?;
        Ok(match server_id {
            Some(id) => conn.execute("DELETE FROM calls WHERE server_id = ?1", params![id])?,
            None => conn.execute("DELETE FROM calls", [])?,
        })
    }

    // ----------------------------------------------------------------- settings

    /// Read a typed setting.
    pub fn get_setting<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        let raw: Option<String> = self
            .conn()?
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()?;
        raw.map(|s| serde_json::from_str(&s).map_err(Error::from))
            .transpose()
    }

    /// Write a typed setting (upsert).
    pub fn set_setting<T: Serialize>(&self, key: &str, value: &T) -> Result<()> {
        self.conn()?.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, serde_json::to_string(value)?],
        )?;
        Ok(())
    }

    /// Remove a setting. Returns whether it existed.
    pub fn delete_setting(&self, key: &str) -> Result<bool> {
        Ok(self
            .conn()?
            .execute("DELETE FROM settings WHERE key = ?1", params![key])?
            > 0)
    }
}

type RowResult<T> = rusqlite::Result<Result<T>>;

fn row_to_server(row: &rusqlite::Row<'_>) -> RowResult<ServerRecord> {
    let id: String = row.get(0)?;
    let name: String = row.get(1)?;
    let spec: String = row.get(2)?;
    let policy: String = row.get(3)?;
    let protocol: String = row.get(4)?;
    Ok((|| {
        Ok(ServerRecord {
            id,
            name,
            spec: serde_json::from_str(&spec)?,
            policy: serde_json::from_str(&policy)?,
            protocol: ProtocolMode::parse(&protocol)
                .ok_or_else(|| Error::NotFound(format!("protocol `{protocol}`")))?,
        })
    })())
}

fn row_to_snapshot(row: &rusqlite::Row<'_>) -> RowResult<SnapshotRecord> {
    let id: String = row.get(0)?;
    let server_id: String = row.get(1)?;
    let digest: String = row.get(2)?;
    let json: String = row.get(3)?;
    Ok((|| {
        Ok(SnapshotRecord {
            id,
            server_id,
            digest,
            snapshot: serde_json::from_str(&json)?,
        })
    })())
}

fn row_to_call(row: &rusqlite::Row<'_>) -> RowResult<CallRecord> {
    let id: String = row.get(0)?;
    let server_id: String = row.get(1)?;
    let kind: String = row.get(2)?;
    let name: String = row.get(3)?;
    let args: String = row.get(4)?;
    let result: Option<String> = row.get(5)?;
    let status: String = row.get(6)?;
    let error: Option<String> = row.get(7)?;
    let elapsed_ms: i64 = row.get(8)?;
    let at: String = row.get(9)?;
    Ok((|| {
        Ok(CallRecord {
            id,
            server_id,
            kind: CallKind::parse(&kind)
                .ok_or_else(|| Error::NotFound(format!("call kind `{kind}`")))?,
            name,
            args: serde_json::from_str::<Value>(&args)?,
            result: result
                .map(|r| serde_json::from_str::<Value>(&r))
                .transpose()?,
            status: CallStatus::parse(&status)
                .ok_or_else(|| Error::NotFound(format!("call status `{status}`")))?,
            error,
            elapsed_ms: u64::try_from(elapsed_ms).unwrap_or(0),
            at: parse_time(&at)?,
        })
    })())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mcp_core::AuthRef;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn stdio(cmd: &str) -> ServerSpec {
        ServerSpec::Stdio {
            command: cmd.into(),
            args: vec![],
            env: BTreeMap::new(),
            cwd: None,
        }
    }

    fn snapshot(desc: &str) -> Snapshot {
        serde_json::from_value(json!({
            "protocolVersion": "2025-11-25",
            "serverInfo": {"name": "mock", "version": "1"},
            "capabilities": {"tools": {}},
            "tools": [{"name": "echo", "description": desc, "inputSchema": {"type": "object"}}],
            "takenAt": "2026-09-11T12:00:00Z"
        }))
        .unwrap()
    }

    #[test]
    fn servers_crud() {
        let store = Store::open_in_memory().unwrap();
        let rec = store
            .add_server("mock", &stdio("mock"), &ServerRequestPolicy::default())
            .unwrap();
        assert_eq!(rec.name, "mock");
        assert_eq!(store.list_servers().unwrap().len(), 1);
        assert_eq!(
            store.find_server_by_name("mock").unwrap().unwrap().id,
            rec.id
        );
        assert!(
            store
                .add_server("mock", &stdio("x"), &ServerRequestPolicy::default())
                .is_err(),
            "unique name"
        );

        let mut updated = rec.clone();
        updated.name = "renamed".into();
        updated.spec = stdio("other");
        store.update_server(&updated).unwrap();
        let back = store.get_server(&rec.id).unwrap().unwrap();
        assert_eq!(back.name, "renamed");
        assert_eq!(back.spec, stdio("other"));

        let same = store
            .get_or_add_server("renamed", &stdio("ignored"))
            .unwrap();
        assert_eq!(same.id, rec.id);
        let fresh = store.get_or_add_server("new", &stdio("n")).unwrap();
        assert_ne!(fresh.id, rec.id);

        assert!(store.delete_server(&rec.id).unwrap());
        assert!(!store.delete_server(&rec.id).unwrap());
        assert!(store.get_server(&rec.id).unwrap().is_none());
        assert!(store.update_server(&updated).is_err());
    }

    #[test]
    fn the_protocol_mode_round_trips() {
        let store = Store::open_in_memory().unwrap();
        let policy = ServerRequestPolicy::default();
        let legacy = store.add_server("old", &stdio("old"), &policy).unwrap();
        assert_eq!(legacy.protocol, ProtocolMode::Legacy);
        let auto = store
            .add_server_with("new", &stdio("new"), &policy, ProtocolMode::Auto)
            .unwrap();
        assert_eq!(auto.protocol, ProtocolMode::Auto);

        let mut modern = auto.clone();
        modern.protocol = ProtocolMode::Modern;
        store.update_server(&modern).unwrap();
        assert_eq!(
            store.get_server(&auto.id).unwrap().unwrap().protocol,
            ProtocolMode::Modern
        );
        let listed: Vec<_> = store
            .list_servers()
            .unwrap()
            .into_iter()
            .map(|s| (s.name, s.protocol))
            .collect();
        assert_eq!(
            listed,
            [
                ("new".to_owned(), ProtocolMode::Modern),
                ("old".to_owned(), ProtocolMode::Legacy)
            ]
        );
    }

    #[test]
    fn a_refusal_is_told_apart_from_a_failed_write() {
        let store = Store::open_in_memory().unwrap();
        let policy = ServerRequestPolicy::default();
        store.add_server("weather", &stdio("a"), &policy).unwrap();
        let taken = store
            .add_server("weather", &stdio("b"), &policy)
            .unwrap_err();
        assert!(taken.is_refusal(), "{taken}");

        let mut env = BTreeMap::new();
        env.insert("GITHUB_TOKEN".to_string(), "ghp_x".to_string());
        let secret = ServerSpec::Stdio {
            command: "gh-mcp".into(),
            args: vec![],
            env,
            cwd: None,
        };
        let secret = store.add_server("gh", &secret, &policy).unwrap_err();
        assert!(secret.is_refusal(), "{secret}");

        // A snapshot whose server row is gone is lost, not refused.
        let snapshot = serde_json::from_value(json!({
            "protocolVersion": "2025-11-25",
            "serverInfo": {"name": "gone", "version": "1"},
            "capabilities": {},
            "takenAt": "2026-09-11T12:00:00Z"
        }))
        .unwrap();
        let orphan = store.add_snapshot("gone", &snapshot).unwrap_err();
        assert!(!orphan.is_refusal(), "{orphan}");
        let io = Error::Sqlite(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_IOERR),
            None,
        ));
        assert!(!io.is_refusal());
        assert!(!Error::Poisoned.is_refusal());
    }

    #[test]
    fn secrets_are_refused() {
        let store = Store::open_in_memory().unwrap();
        let mut headers = BTreeMap::new();
        headers.insert("Authorization".to_string(), "Bearer abc".to_string());
        let spec = ServerSpec::Http {
            url: "https://example.com/mcp".into(),
            headers,
            auth: AuthRef::None,
        };
        let err = store
            .add_server("remote", &spec, &ServerRequestPolicy::default())
            .unwrap_err();
        assert!(matches!(err, Error::SecretInSpec(_)), "{err}");

        let mut env = BTreeMap::new();
        env.insert("GITHUB_TOKEN".to_string(), "ghp_x".to_string());
        let spec = ServerSpec::Stdio {
            command: "gh-mcp".into(),
            args: vec![],
            env,
            cwd: None,
        };
        assert!(matches!(
            store.add_server("gh", &spec, &ServerRequestPolicy::default()),
            Err(Error::SecretInSpec(_))
        ));

        // Keyring references are fine.
        let spec = ServerSpec::Http {
            url: "https://example.com/mcp".into(),
            headers: BTreeMap::new(),
            auth: AuthRef::Bearer {
                keyring_id: "remote-token".into(),
            },
        };
        assert!(
            store
                .add_server("remote", &spec, &ServerRequestPolicy::default())
                .is_ok()
        );
        assert_eq!(store.list_servers().unwrap().len(), 1);
    }

    #[test]
    fn snapshots_and_cascade() {
        let store = Store::open_in_memory().unwrap();
        let server = store.get_or_add_server("mock", &stdio("mock")).unwrap();
        assert!(store.latest_snapshot(&server.id).unwrap().is_none());
        let first = store
            .add_snapshot(&server.id, &snapshot("v1"))
            .unwrap()
            .unwrap();
        let mut later = snapshot("v2");
        later.taken_at += time::Duration::minutes(1);
        let second = store.add_snapshot(&server.id, &later).unwrap().unwrap();
        assert_ne!(first.digest, second.digest);

        let latest = store.latest_snapshot(&server.id).unwrap().unwrap();
        assert_eq!(latest.id, second.id);
        assert_eq!(latest.snapshot.tools[0].description.as_deref(), Some("v2"));
        assert_eq!(
            store.get_snapshot(&first.id).unwrap().unwrap().digest,
            first.digest
        );
        let list = store.list_snapshots(&server.id, 10).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, second.id);
        assert_eq!(store.list_snapshots(&server.id, 1).unwrap().len(), 1);

        store.delete_server(&server.id).unwrap();
        assert!(store.get_snapshot(&first.id).unwrap().is_none(), "cascade");
    }

    #[test]
    fn a_snapshot_missing_a_list_is_never_a_baseline() {
        let store = Store::open_in_memory().unwrap();
        let server = store.get_or_add_server("mock", &stdio("mock")).unwrap();
        let mut partial = snapshot("v1");
        partial.list_failures.push(mcp_core::ListFailure {
            method: mcp_core::list_method::RESOURCES.into(),
            error: "server error -32603: resources are unavailable".into(),
        });
        assert!(store.add_snapshot(&server.id, &partial).unwrap().is_none());
        assert!(store.latest_snapshot(&server.id).unwrap().is_none());

        let complete = store
            .add_snapshot(&server.id, &snapshot("v1"))
            .unwrap()
            .unwrap();
        assert!(store.add_snapshot(&server.id, &partial).unwrap().is_none());
        assert_eq!(
            store.latest_snapshot(&server.id).unwrap().unwrap().id,
            complete.id
        );
        assert_eq!(store.list_snapshots(&server.id, 10).unwrap().len(), 1);
    }

    #[test]
    fn calls_history() {
        let store = Store::open_in_memory().unwrap();
        let a = store.get_or_add_server("a", &stdio("a")).unwrap();
        let b = store.get_or_add_server("b", &stdio("b")).unwrap();
        let ok = store
            .record_call(NewCall {
                server_id: a.id.clone(),
                kind: CallKind::Tool,
                name: "echo".into(),
                args: json!({"text": "hi"}),
                result: Some(json!({"content": []})),
                status: CallStatus::Ok,
                error: None,
                elapsed_ms: 12,
            })
            .unwrap();
        store
            .record_call(NewCall {
                server_id: b.id.clone(),
                kind: CallKind::Resource,
                name: "mock://x".into(),
                args: Value::Null,
                result: None,
                status: CallStatus::Failed,
                error: Some("boom".into()),
                elapsed_ms: 3,
            })
            .unwrap();
        assert_eq!(store.list_calls(None, 10).unwrap().len(), 2);
        let only_a = store.list_calls(Some(&a.id), 10).unwrap();
        assert_eq!(only_a.len(), 1);
        assert_eq!(only_a[0], ok);
        assert_eq!(store.get_call(&ok.id).unwrap().unwrap().args["text"], "hi");
        // One call goes alone; the others stay.
        let extra = store
            .record_call(NewCall {
                server_id: a.id.clone(),
                kind: CallKind::Tool,
                name: "extra".into(),
                args: Value::Null,
                result: None,
                status: CallStatus::Ok,
                error: None,
                elapsed_ms: 1,
            })
            .unwrap();
        assert!(store.delete_call(&extra.id).unwrap());
        assert!(store.get_call(&extra.id).unwrap().is_none());
        assert!(!store.delete_call(&extra.id).unwrap(), "already gone");
        assert_eq!(store.list_calls(Some(&a.id), 10).unwrap().len(), 1);
        let failed = &store.list_calls(Some(&b.id), 10).unwrap()[0];
        assert_eq!(failed.status, CallStatus::Failed);
        assert_eq!(failed.error.as_deref(), Some("boom"));
        assert_eq!(store.clear_calls(Some(&a.id)).unwrap(), 1);
        assert_eq!(store.clear_calls(None).unwrap(), 1);
        assert!(store.list_calls(None, 10).unwrap().is_empty());
    }

    #[test]
    fn calls_order_by_time_not_text() {
        let store = Store::open_in_memory().unwrap();
        let a = store.get_or_add_server("a", &stdio("a")).unwrap();
        // Within one second RFC 3339 fractions differ in width, and as text
        // `.1Z` sorts after `.125Z`.
        let mut ids = Vec::new();
        for at in [
            "2026-01-01T00:00:05.1Z",
            "2026-01-01T00:00:05.12Z",
            "2026-01-01T00:00:05.125Z",
        ] {
            let call = store
                .record_call(NewCall {
                    server_id: a.id.clone(),
                    kind: CallKind::Tool,
                    name: at.into(),
                    args: Value::Null,
                    result: None,
                    status: CallStatus::Ok,
                    error: None,
                    elapsed_ms: 1,
                })
                .unwrap();
            store
                .conn()
                .unwrap()
                .execute(
                    "UPDATE calls SET at = ?1 WHERE id = ?2",
                    params![at, call.id],
                )
                .unwrap();
            ids.push(call.id);
        }
        ids.reverse();
        let listed: Vec<String> = store
            .list_calls(Some(&a.id), 10)
            .unwrap()
            .into_iter()
            .map(|call| call.id)
            .collect();
        assert_eq!(listed, ids, "newest first");
    }

    #[test]
    fn settings_round_trip() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.get_setting::<String>("theme").unwrap(), None);
        store.set_setting("theme", &"dark").unwrap();
        store.set_setting("theme", &"light").unwrap();
        assert_eq!(
            store.get_setting::<String>("theme").unwrap().as_deref(),
            Some("light")
        );
        store
            .set_setting("drawer", &json!({"open": true, "height": 240}))
            .unwrap();
        assert_eq!(
            store.get_setting::<Value>("drawer").unwrap().unwrap()["height"],
            240
        );
        assert!(store.delete_setting("theme").unwrap());
        assert!(!store.delete_setting("theme").unwrap());
    }

    #[test]
    fn opens_on_disk_and_reopens() {
        let dir = std::env::temp_dir().join(format!("mcp-store-test-{}", uuid::Uuid::now_v7()));
        let path = dir.join("nested").join("coco.db");
        {
            let store = Store::open(&path).unwrap();
            store.get_or_add_server("persisted", &stdio("p")).unwrap();
        }
        let store = Store::open(&path).unwrap();
        assert!(store.find_server_by_name("persisted").unwrap().is_some());
        drop(store);
        let _ = std::fs::remove_dir_all(dir);
        assert!(Store::default_path().is_some());
    }
}
