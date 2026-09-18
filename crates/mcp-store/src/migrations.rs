//! Schema migrations, applied in order by `rusqlite_migration`. Never edit a
//! shipped migration; append a new `M::up`.

use rusqlite_migration::{M, Migrations};

// V1's `collections` table was for named saved requests, which nothing ever
// offered: History replays a call and loads it back into the form. The table
// stays, empty, because a shipped migration is never edited.
const V1: &str = r#"
CREATE TABLE servers (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    spec        TEXT NOT NULL,          -- ServerSpec JSON (keyring refs only)
    policy      TEXT NOT NULL,          -- ServerRequestPolicy JSON
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

CREATE TABLE snapshots (
    id          TEXT PRIMARY KEY,
    server_id   TEXT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    digest      TEXT NOT NULL,
    json        TEXT NOT NULL,          -- Snapshot JSON
    taken_at    TEXT NOT NULL
);
CREATE INDEX snapshots_server_taken ON snapshots(server_id, taken_at DESC);

CREATE TABLE calls (
    id          TEXT PRIMARY KEY,
    server_id   TEXT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    kind        TEXT NOT NULL,          -- tool | resource | prompt
    name        TEXT NOT NULL,
    args        TEXT NOT NULL,          -- JSON
    result      TEXT,                   -- JSON, NULL when the call failed
    status      TEXT NOT NULL,          -- ok | tool_error | failed
    error       TEXT,                   -- message when status = failed
    elapsed_ms  INTEGER NOT NULL,
    at          TEXT NOT NULL
);
CREATE INDEX calls_server_at ON calls(server_id, at DESC);

CREATE TABLE collections (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    items       TEXT NOT NULL,          -- JSON array of CollectionItem
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

CREATE TABLE settings (
    key         TEXT PRIMARY KEY,
    value       TEXT NOT NULL           -- JSON
);
"#;

// The library default used to refuse every server request, and rows written
// by the command-line import or by `coco --db` carry its exact JSON. No
// screen can change a policy, so those rows move to the current default,
// which asks a person; any other policy is left as it is.
const V2: &str = r#"
UPDATE servers
SET policy = '{"sampling":{"mode":"prompt"},"elicitation":"prompt","roots":{"mode":"prompt"},"prompt_timeout":120}'
WHERE policy = '{"sampling":{"mode":"reject"},"elicitation":"decline","roots":{"mode":"fixed","roots":[]},"prompt_timeout":120}';
"#;

// When a server row was created or last changed was written on every insert
// and update, and read by nothing.
const V3: &str = r#"
ALTER TABLE servers DROP COLUMN created_at;
ALTER TABLE servers DROP COLUMN updated_at;
"#;

// The protocol era a server is connected in (`ProtocolMode`). Every server
// saved before it connected with the handshake, which is `legacy`.
const V4: &str = r#"
ALTER TABLE servers ADD COLUMN protocol TEXT NOT NULL DEFAULT 'legacy';
"#;

const MIGRATIONS_SLICE: &[M<'static>] = &[M::up(V1), M::up(V2), M::up(V3), M::up(V4)];

/// All migrations, oldest first.
pub const MIGRATIONS: Migrations<'static> = Migrations::from_slice(MIGRATIONS_SLICE);

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use mcp_core::{ElicitationPolicy, RootsPolicy, SamplingPolicy, ServerRequestPolicy};
    use rusqlite::{Connection, params};

    use super::*;

    #[test]
    fn migrations_are_valid() {
        MIGRATIONS.validate().unwrap();
    }

    #[test]
    fn servers_saved_with_the_old_refusing_default_now_ask() {
        let old = ServerRequestPolicy {
            sampling: SamplingPolicy::Reject,
            elicitation: ElicitationPolicy::Decline,
            roots: RootsPolicy::Fixed { roots: Vec::new() },
            prompt_timeout: Duration::from_secs(120),
        };
        let old_json = serde_json::to_string(&old).unwrap();
        let new_json = serde_json::to_string(&ServerRequestPolicy::default()).unwrap();
        // The literals must be byte for byte what serde writes, or the
        // update matches nothing.
        assert!(
            V2.contains(&format!("WHERE policy = '{old_json}'")),
            "{old_json}"
        );
        assert!(
            V2.contains(&format!("SET policy = '{new_json}'")),
            "{new_json}"
        );

        let chosen = ServerRequestPolicy {
            prompt_timeout: Duration::from_secs(5),
            ..old.clone()
        };
        let mut conn = Connection::open_in_memory().unwrap();
        MIGRATIONS.to_version(&mut conn, 1).unwrap();
        let insert = |conn: &Connection, id: &str, policy: &ServerRequestPolicy| {
            conn.execute(
                "INSERT INTO servers (id, name, spec, policy, created_at, updated_at) \
                 VALUES (?1, ?1, '{}', ?2, '', '')",
                params![id, serde_json::to_string(policy).unwrap()],
            )
            .unwrap();
        };
        insert(&conn, "imported", &old);
        insert(&conn, "chosen", &chosen);

        MIGRATIONS.to_latest(&mut conn).unwrap();
        let policy = |id: &str| -> ServerRequestPolicy {
            let json: String = conn
                .query_row(
                    "SELECT policy FROM servers WHERE id = ?1",
                    params![id],
                    |row| row.get(0),
                )
                .unwrap();
            serde_json::from_str(&json).unwrap()
        };
        assert_eq!(policy("imported"), ServerRequestPolicy::default());
        assert_eq!(policy("chosen"), chosen, "a policy that differs is kept");
    }

    #[test]
    fn servers_saved_before_the_protocol_column_are_legacy() {
        let mut conn = Connection::open_in_memory().unwrap();
        MIGRATIONS.to_version(&mut conn, 3).unwrap();
        conn.execute(
            "INSERT INTO servers (id, name, spec, policy) VALUES ('old', 'old', '{}', '{}')",
            [],
        )
        .unwrap();
        MIGRATIONS.to_latest(&mut conn).unwrap();
        let protocol: String = conn
            .query_row("SELECT protocol FROM servers WHERE id = 'old'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(protocol, mcp_core::ProtocolMode::Legacy.as_str());
    }
}
