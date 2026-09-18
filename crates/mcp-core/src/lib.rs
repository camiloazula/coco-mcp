//! UI-free MCP client core: server specs, sessions, snapshots, calls and the
//! event stream consumed by the app's log drawer and the `coco` CLI.
//!
//! This crate wraps the official `rmcp` client. It must never depend on
//! `gpui` or `gpui-kit`; everything here is reachable from a test.

#![forbid(unsafe_code)]
// unwrap()/expect() are denied in shipped code but fine inside tests.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod command;
mod error;
pub mod event;
pub mod handler;
pub mod lifecycle;
mod listen;
pub mod session;
pub mod snapshot;
mod spec;
pub mod transport;

pub use command::{command_line, split_command_line};
pub use error::Error;
pub use event::{
    ConnectionState, Direction, ElicitationAction, ElicitationMode, Event, EventCategory,
    EventKind, EventSink, ListKind, Root, ServerRequest, ServerRequestKind, ServerResponse,
};
pub use handler::{ElicitationPolicy, RootsPolicy, SamplingPolicy, ServerRequestPolicy};
pub use lifecycle::{Era, LEGACY_VERSION, MODERN_VERSION};
pub use session::{
    CallOutcome, Completion, CompletionTarget, LOG_LEVELS, MAX_INPUT_ROUNDS, PromptOutcome,
    RequestControl, ResourceOutcome, Session, SessionOptions,
};
pub use snapshot::{
    ListFailure, Prompt, PromptArgument, Resource, ResourceTemplate, Snapshot, Tool, list_method,
};
pub use spec::{AuthRef, ProtocolMode, ServerSpec};

/// Convenience alias used throughout the core crates.
pub type Result<T> = std::result::Result<T, Error>;
