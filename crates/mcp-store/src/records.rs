//! Row types returned by the store.

use mcp_core::{ProtocolMode, ServerRequestPolicy, ServerSpec, Snapshot};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;

/// A saved server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerRecord {
    /// UUID v7.
    pub id: String,
    /// Unique display name.
    pub name: String,
    /// How to connect.
    pub spec: ServerSpec,
    /// How server-initiated requests are answered.
    pub policy: ServerRequestPolicy,
    /// Which protocol era the server is connected in.
    #[serde(default)]
    pub protocol: ProtocolMode,
}

/// A stored snapshot including its content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotRecord {
    /// UUID v7.
    pub id: String,
    /// Owning server.
    pub server_id: String,
    /// `Snapshot::digest()` at insert time.
    pub digest: String,
    /// The snapshot.
    pub snapshot: Snapshot,
}

/// A snapshot row without its content (for lists).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotSummary {
    /// UUID v7.
    pub id: String,
    /// Owning server.
    pub server_id: String,
    /// Content digest.
    pub digest: String,
    /// When it was taken.
    #[serde(with = "time::serde::rfc3339")]
    pub taken_at: OffsetDateTime,
}

/// What kind of request a call was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallKind {
    /// `tools/call`.
    Tool,
    /// `resources/read`.
    Resource,
    /// `prompts/get`.
    Prompt,
}

impl CallKind {
    /// Column value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tool => "tool",
            Self::Resource => "resource",
            Self::Prompt => "prompt",
        }
    }

    /// Parse a column value.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "tool" => Some(Self::Tool),
            "resource" => Some(Self::Resource),
            "prompt" => Some(Self::Prompt),
            _ => None,
        }
    }
}

/// How a call ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallStatus {
    /// The server answered successfully.
    Ok,
    /// The server answered with `isError: true`.
    ToolError,
    /// Transport or JSON-RPC failure; see `error`.
    Failed,
}

impl CallStatus {
    /// Column value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::ToolError => "tool_error",
            Self::Failed => "failed",
        }
    }

    /// Parse a column value.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "ok" => Some(Self::Ok),
            "tool_error" => Some(Self::ToolError),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// Input to [`crate::Store::record_call`].
#[derive(Debug, Clone, PartialEq)]
pub struct NewCall {
    /// Owning server.
    pub server_id: String,
    /// Request kind.
    pub kind: CallKind,
    /// Tool name, resource URI or prompt name.
    pub name: String,
    /// Arguments sent.
    pub args: Value,
    /// Raw result, when the server answered.
    pub result: Option<Value>,
    /// Outcome.
    pub status: CallStatus,
    /// Error text when `status == Failed`.
    pub error: Option<String>,
    /// Round-trip time in milliseconds.
    pub elapsed_ms: u64,
}

/// A stored call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallRecord {
    /// UUID v7.
    pub id: String,
    /// Owning server.
    pub server_id: String,
    /// Request kind.
    pub kind: CallKind,
    /// Tool name, resource URI or prompt name.
    pub name: String,
    /// Arguments sent.
    pub args: Value,
    /// Raw result, when the server answered.
    pub result: Option<Value>,
    /// Outcome.
    pub status: CallStatus,
    /// Error text when `status == Failed`.
    pub error: Option<String>,
    /// Round-trip time in milliseconds.
    pub elapsed_ms: u64,
    /// When the call was made.
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
}
