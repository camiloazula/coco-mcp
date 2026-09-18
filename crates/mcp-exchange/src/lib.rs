//! The formats Coco MCP shares with other tools: what a copy puts on
//! the clipboard, what an export writes to a file, and what an import reads
//! back.
//!
//! A debugger is only as useful as the data you can move in and out of it,
//! so the formats live here rather than in the views: the app and the CLI
//! produce byte-identical output, read the same files, and there is one
//! place that decides what happens to a credential. Nothing here touches the
//! keyring, and no format here carries a secret: a bearer token is always
//! written as a placeholder.
//!
//! UI-free: must never depend on `gpui` or `gpui-kit`.

#![forbid(unsafe_code)]
// unwrap()/expect() are denied in shipped code but fine inside tests.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod config;
mod files;
mod request;
mod value;

pub use config::{ClientConfig, ImportedConfig, ImportedServer, client_config, read_client_config};
pub use files::{LogLine, diff_markdown, file_name, history_jsonl, log_jsonl, snapshot_json};
/// A stdio server's command line. Implemented in `mcp-core`, whose
/// [`mcp_core::ServerSpec::label`] prints the same quoting.
pub use mcp_core::{command_line, split_command_line};
pub use request::{CURL_TOKEN_VAR, Curl, Frame, curl, json_rpc, wire_message};
pub use value::{Segment, copy_text, json_head, json_size, path_string, pretty, resolve};
