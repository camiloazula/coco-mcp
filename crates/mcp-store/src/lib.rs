//! SQLite persistence: servers, snapshots, call history, settings.
//!
//! Secrets never enter this database. A [`mcp_core::ServerSpec`] only carries keyring
//! references ([`mcp_core::AuthRef`]); `Authorization` headers are refused at
//! insert time so a bearer token cannot be smuggled in as a plain header.
//!
//! The API is synchronous (rusqlite is), cheap to clone, and safe to call
//! from `tokio::task::spawn_blocking`. UI-free: must never depend on `gpui`
//! or `gpui-kit`.

#![forbid(unsafe_code)]
// unwrap()/expect() are denied in shipped code but fine inside tests.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod migrations;
mod records;
mod store;

pub use records::{
    CallKind, CallRecord, CallStatus, NewCall, ServerRecord, SnapshotRecord, SnapshotSummary,
};
pub use store::{NewServer, Store};

/// Errors produced by the store.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// SQLite failure.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    /// Schema migration failure.
    #[error("migration failed: {0}")]
    Migration(#[from] rusqlite_migration::Error),
    /// A stored JSON column could not be (de)serialized.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// Filesystem failure creating the database directory.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// The spec would persist a secret (e.g. an `Authorization` header).
    #[error("refusing to store a secret: {0}")]
    SecretInSpec(String),
    /// A row that must exist is missing.
    #[error("not found: {0}")]
    NotFound(String),
    /// The connection mutex was poisoned by a panic elsewhere.
    #[error("store lock poisoned")]
    Poisoned,
    /// A timestamp column held text that is not RFC 3339.
    #[error("bad timestamp in column: {0}")]
    Timestamp(String),
}

impl Error {
    /// Whether the store turned a write down for what it holds rather than
    /// failing to make it: a spec with a secret in it, or a value a unique
    /// column already has, such as a server name that is taken. The same
    /// write is refused again; nothing was lost.
    pub fn is_refusal(&self) -> bool {
        match self {
            Self::SecretInSpec(_) => true,
            Self::Sqlite(e) => {
                e.sqlite_extended_error_code() == Some(rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE)
            }
            _ => false,
        }
    }
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, Error>;
