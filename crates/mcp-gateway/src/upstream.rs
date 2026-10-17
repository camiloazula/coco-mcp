//! The server behind a path, as the gateway sees it: something messages are
//! sent to, and a stream of what it says. A stdio process and an HTTP
//! session are two ways to be one; a test supplies a third.

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use serde_json::Value;
use thiserror::Error;
use tokio::sync::mpsc;

pub use crate::http::{Authorizer, Bearer, HttpConnector, NoAuth};
pub use crate::stdio::{StdioConnector, stdio_over_streams};

/// Messages a server can have waiting before the gateway reads them.
pub(crate) const INCOMING: usize = 256;

/// Longest message kept, in bytes: a line a stdio server wrote, an SSE event
/// or a JSON response body. A server that sends more is cut off.
pub(crate) const MAX_MESSAGE: usize = 32 * 1024 * 1024;

/// A message the agent sent: parsed to be routed, and as it arrived to be
/// forwarded to a server that takes the bytes as they are.
#[derive(Debug, Clone)]
pub struct Frame {
    /// The message.
    pub value: Value,
    /// The bytes it came as.
    pub raw: Bytes,
}

/// What of the agent's request goes on to an HTTP server: its `Mcp-*`
/// headers, except the session id, which the gateway keeps for itself.
#[derive(Debug, Clone, Default)]
pub struct RequestContext {
    /// Header names and values.
    pub headers: Vec<(String, String)>,
}

/// Why a message could not be sent on.
#[derive(Debug, Error)]
pub enum UpstreamError {
    /// An HTTP server answered with an error. The agent is told the same.
    #[error("the server answered HTTP {status}")]
    Http {
        /// The status code.
        status: u16,
        /// The headers worth passing on (type, challenge, retry).
        headers: Vec<(String, String)>,
        /// What it said.
        body: Vec<u8>,
    },
    /// The server's side is gone.
    #[error("the server's session ended")]
    Closed,
    /// It could not be reached, started or understood.
    #[error("{0}")]
    Failed(String),
}

/// An open server.
#[async_trait]
pub trait Upstream: Send + Sync + 'static {
    /// Send one message on.
    async fn send(&self, frame: &Frame, context: &RequestContext) -> Result<(), UpstreamError>;

    /// End the server's session: stop the process, or close the HTTP session.
    async fn close(&self);
}

/// A server just opened, and what it will say.
#[allow(missing_debug_implementations)]
pub struct Opened {
    /// Where to send messages.
    pub upstream: Arc<dyn Upstream>,
    /// Everything the server says, in the order it arrives, however it got
    /// here. It ends when the server does.
    pub incoming: mpsc::Receiver<Value>,
}

/// Where to send what the server writes to its standard error.
pub type StderrSink = Arc<dyn Fn(String) + Send + Sync>;

/// How to open a server: once for each agent that connects.
#[async_trait]
pub trait Connector: Send + Sync + 'static {
    /// Open the server.
    async fn open(&self, stderr: StderrSink) -> Result<Opened, UpstreamError>;
}
