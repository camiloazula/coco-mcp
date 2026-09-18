//! Error type for `mcp-core`.

use std::time::Duration;

/// Errors produced by the core client.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The server spec could not be turned into a transport.
    #[error("invalid server spec: {0}")]
    InvalidSpec(String),
    /// Transport-level failure (spawn, connect, I/O).
    #[error("transport error: {0}")]
    Transport(String),
    /// The session did not start: the handshake or `server/discover` failed.
    #[error("connect failed: {0}")]
    Initialize(String),
    /// The server returned a JSON-RPC error.
    #[error("server error {code}: {message}")]
    Server {
        /// JSON-RPC error code.
        code: i64,
        /// Human-readable message from the server.
        message: String,
        /// Optional structured data.
        data: Option<serde_json::Value>,
    },
    /// The request did not complete in time.
    #[error("request timed out after {0:?}")]
    Timeout(Duration),
    /// The connection is closed.
    #[error("connection closed")]
    Closed,
    /// The caller cancelled the request; the server was told with
    /// `notifications/cancelled`.
    #[error("cancelled")]
    Cancelled,
    /// The server answered `401`/`403`; `challenge` is its `WWW-Authenticate`
    /// header when present. Configure auth (bearer or OAuth) and reconnect.
    #[error("authorization required{}", challenge.as_deref().map(|c| format!(": {c}")).unwrap_or_default())]
    AuthRequired {
        /// The `WWW-Authenticate` challenge, if any.
        challenge: Option<String>,
    },
    /// Arguments were not acceptable before sending.
    #[error("invalid arguments: {0}")]
    InvalidArguments(String),
    /// Something asked of the client was turned down: an input request the
    /// policy refuses, or a subscription the server did not accept.
    #[error("refused: {0}")]
    Refused(String),
    /// The server kept answering `input_required` past the round limit.
    #[error("the server still asked for input after {0} rounds")]
    InputRounds(usize),
    /// JSON (de)serialization failure.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl Error {
    /// Whether the server said the resource does not exist: `-32602` with
    /// the URI in its data from 2026-07-28 on, `-32002` from older servers.
    pub fn is_resource_not_found(&self) -> bool {
        match self {
            Self::Server { code: -32002, .. } => true,
            Self::Server {
                code: -32602, data, ..
            } => data.as_ref().and_then(|d| d.get("uri")).is_some(),
            _ => false,
        }
    }
}

impl From<rmcp::ServiceError> for Error {
    fn from(e: rmcp::ServiceError) -> Self {
        match e {
            rmcp::ServiceError::McpError(data) => Self::Server {
                code: i64::from(data.code.0),
                message: data.message.into_owned(),
                data: data.data,
            },
            rmcp::ServiceError::TransportClosed => Self::Closed,
            rmcp::ServiceError::Timeout { timeout } => Self::Timeout(timeout),
            other => Self::Transport(other.to_string()),
        }
    }
}
