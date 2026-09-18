//! What each copy and export action produces.
//!
//! Kept apart from the handlers that fire them so every payload is a plain
//! function of the model: the tests build them without a save panel, and the
//! menu bar, the palette and the toolbar button can never disagree about
//! what "copy the request" means.

use futures::channel::oneshot;
use gpui_kit::{App, AppContext as _, Context};
use mcp_exchange::{ClientConfig, Curl, LogLine};
use mcp_store::CallKind;
use serde_json::{Map, Value, json};

use crate::state::{AppState, Dir, HistoryLoad, Mode, Screen, ServerEntry};
use crate::views::Workspace;
use crate::views::history::method;

mod actions;
mod import;

/// A file the app can write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Export {
    /// Everything the server advertised, as `mcp-diff` compares it.
    Snapshot,
    /// The session's wire messages.
    Log,
    /// Recorded calls.
    History,
    /// The `mcpServers` block for other clients.
    Config,
}

impl Export {
    /// What the confirmation and the palette call it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Snapshot => "snapshot",
            Self::Log => "log",
            Self::History => "history",
            Self::Config => "config",
        }
    }
}

impl Workspace {
    /// The JSON-RPC message the current screen would send, built from the
    /// form as it stands rather than from a validated call.
    pub fn request_message(&mut self, cx: &mut Context<Self>) -> Option<Value> {
        if self.state.read(cx).screen != Screen::Detail {
            return None;
        }
        let (mode, name) = {
            let s = self.state.read(cx);
            (s.mode, s.selected_name()?)
        };
        let (method, params) = match mode {
            Mode::Tools => {
                let form = self.selection.form.clone()?;
                let arguments = form.update(cx, |f, cx| f.current_json(cx));
                (
                    "tools/call",
                    json!({ "name": name, "arguments": arguments }),
                )
            }
            Mode::Resources => (
                "resources/read",
                json!({ "uri": self.expand_template(&name, cx) }),
            ),
            Mode::Prompts => {
                let mut arguments = Map::new();
                for (arg, input) in &self.selection.args {
                    let value = input.read(cx).value().to_string();
                    if !value.is_empty() {
                        arguments.insert(arg.clone(), Value::String(value));
                    }
                }
                (
                    "prompts/get",
                    json!({ "name": name, "arguments": arguments }),
                )
            }
            // The Server view sends nothing.
            Mode::Server => return None,
            Mode::History => {
                let record = self.state.read(cx).selected_call()?.clone();
                let params = match record.kind {
                    CallKind::Tool | CallKind::Prompt => {
                        json!({ "name": record.name, "arguments": record.args })
                    }
                    CallKind::Resource => json!({ "uri": record.name }),
                };
                (method(record.kind), params)
            }
        };
        Some(mcp_exchange::json_rpc(method, &params))
    }

    /// The same message as a `curl` command. `None` for a stdio server,
    /// which has no endpoint to post to.
    pub fn request_curl(&mut self, cx: &mut Context<Self>) -> Option<String> {
        let body = self.request_message(cx)?;
        let spec = self.state.read(cx).server()?.record.spec.clone();
        let mcp_core::ServerSpec::Http { url, headers, auth } = &spec else {
            return None;
        };
        Some(mcp_exchange::curl(Curl {
            url,
            headers,
            auth,
            body: &body,
        }))
    }

    /// The whole result under the current selection, pretty-printed.
    pub fn response_json(&self, cx: &App) -> Option<String> {
        let state = self.state.read(cx);
        let raw = match (state.mode, state.response()) {
            (_, Some(response)) => &response.raw,
            // A history row without a replay shows its stored result.
            (Mode::History, None) => state.selected_call()?.result.as_ref()?,
            _ => return None,
        };
        let empty = raw.as_object().is_some_and(|o| o.is_empty());
        (!empty).then(|| mcp_exchange::pretty(raw))
    }

    /// The client configuration for the selected server, or for all of them
    /// when none is selected.
    pub fn client_config(&self, cx: &App) -> ClientConfig {
        let state = self.state.read(cx);
        config_of(state, state.server())
    }

    /// The bearer token of the selected server, read from the secret store.
    ///
    /// The only path by which a live credential leaves the keyring, and it
    /// is reached only from an action whose name says so. Blocking: the
    /// action reads it through [`Self::copy_bearer_token`], off the GPUI
    /// thread.
    pub fn bearer_token(&self, cx: &App) -> Option<String> {
        let state = self.state.read(cx);
        let server = state.server()?;
        let id = crate::state::keyring_id(&server.record.spec)?;
        state.secrets.get(&id).ok().flatten()
    }

    /// The suggested file name and the bytes of an export, or `None` when
    /// there is nothing to write.
    pub fn export(&self, kind: Export, cx: &App) -> Option<(String, Vec<u8>)> {
        let state = self.state.read(cx);
        export_of(state, kind, state.server())
    }

    /// An export once what it needs is read. A history export of a server
    /// whose stored calls are unread waits for them, and stays that server's
    /// when another is selected meanwhile. Resolves to `None` when there is
    /// nothing to write; dropped when the server was deleted first.
    pub fn export_ready(
        &mut self,
        kind: Export,
        cx: &mut Context<Self>,
    ) -> oneshot::Receiver<Option<(String, Vec<u8>)>> {
        let (done, export) = oneshot::channel();
        let unread = (kind == Export::History)
            .then(|| {
                let state = self.state.read(cx);
                let ix = state.selected_server?;
                let server = state.servers.get(ix)?;
                (server.history_load() != HistoryLoad::Read).then(|| (ix, server.record.id.clone()))
            })
            .flatten();
        let Some((ix, id)) = unread else {
            let _ = done.send(self.export(kind, cx));
            return export;
        };
        let ready = self.state.update(cx, |s, cx| s.history_ready(ix, cx));
        cx.spawn(async move |this, cx| {
            if ready.await.is_err() {
                return;
            }
            let _ = this.update(cx, |ws, cx| {
                let state = ws.state.read(cx);
                // Looked up again by id: the list may have changed meanwhile.
                if let Some(server) = state.servers.iter().find(|s| s.record.id == id) {
                    let _ = done.send(export_of(state, kind, Some(server)));
                }
            });
        })
        .detach();
        export
    }
}

/// The client configuration for `server`, or for every server without one.
fn config_of(state: &AppState, server: Option<&ServerEntry>) -> ClientConfig {
    let records: Vec<_> = match server {
        Some(server) => vec![server.record.clone()],
        None => state.servers.iter().map(|s| s.record.clone()).collect(),
    };
    mcp_exchange::client_config(&records)
}

/// The suggested file name and the bytes of an export of `server`, or `None`
/// when there is nothing to write.
fn export_of(
    state: &AppState,
    kind: Export,
    server: Option<&ServerEntry>,
) -> Option<(String, Vec<u8>)> {
    let name = server.map(|s| s.record.name.as_str()).unwrap_or("");
    let (contents, extension) = match kind {
        Export::Snapshot => (mcp_exchange::snapshot_json(server?.snapshot()?), "json"),
        Export::Log => {
            let log = server?.log();
            if log.is_empty() {
                return None;
            }
            let lines: Vec<LogLine<'_>> = log
                .iter()
                .map(|row| LogLine {
                    at: row.at,
                    direction: match row.dir {
                        Dir::Out => "out",
                        Dir::In => "in",
                        Dir::Note => "note",
                    },
                    method: &row.method,
                    payload: &row.payload,
                })
                .collect();
            (mcp_exchange::log_jsonl(&lines), "jsonl")
        }
        Export::History => {
            let history = server?.history();
            if history.is_empty() {
                return None;
            }
            (mcp_exchange::history_jsonl(history), "jsonl")
        }
        Export::Config => (config_of(state, server).text(), "json"),
    };
    Some((
        mcp_exchange::file_name(name, kind.label(), extension),
        contents.into_bytes(),
    ))
}
