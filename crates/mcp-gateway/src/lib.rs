//! A local gateway between an agent and the MCP servers Coco knows.
//!
//! The agent connects to `http://127.0.0.1:<port>/<namespace>` as it would
//! to the server itself. The gateway opens the real server behind that
//! path (a process for a stdio server, an HTTP session for an HTTP one) and
//! passes every JSON-RPC message through, in both directions and unchanged,
//! while telling an [`Observer`] what went by. It never interprets the
//! protocol: it does not negotiate versions, answer requests or rewrite
//! names, so what the agent sees is what the server sent.
//!
//! Streamable HTTP is spoken to the agent (a request is answered on a
//! `text/event-stream`, anything else with `202`, and a `GET` opens the
//! stream for what the server says on its own). Each agent connection gets
//! its own upstream session, opened when the agent first posts and closed
//! on `DELETE`, when the server ends, when the agent is idle for too long or
//! when the gateway stops.
//!
//! The gateway calls servers with the user's stored credentials, so it
//! answers only on `127.0.0.1`, only with the access token it was started
//! with, and only for the `Host` and `Origin` of this machine.
//!
//! UI-free: must never depend on `gpui` or `gpui-kit`.

#![forbid(unsafe_code)]
// unwrap()/expect() are denied in shipped code but fine inside tests.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod frame;
mod http;
mod observe;
mod relay;
mod server;
mod stdio;
mod upstream;

pub use observe::{ClientInfo, Observer, Silent, Traffic};
pub use server::{Gateway, RelayServer, valid_namespace};
pub use upstream::{
    Authorizer, Bearer, Connector, Frame, HttpConnector, NoAuth, Opened, RequestContext,
    StderrSink, StdioConnector, Upstream, UpstreamError, stdio_over_streams,
};

/// Which way a message went, as seen from the agent: toward the server, or
/// toward the agent.
pub use mcp_core::Direction;
