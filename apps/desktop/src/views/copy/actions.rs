//! The copy and export actions: what each clipboard entry and save panel
//! writes.

use super::*;

impl Workspace {
    /// Copy the whole response, or say why there is nothing to copy.
    pub fn copy_response(&mut self, cx: &mut Context<Self>) {
        match self.response_json(cx) {
            Some(text) => crate::clip::copy(text, "response", cx),
            None => crate::clip::announce_nothing("No response yet", cx),
        }
    }

    /// Copy the current request as a JSON-RPC message.
    pub fn copy_request(&mut self, cx: &mut Context<Self>) {
        match self.request_message(cx) {
            Some(message) => crate::clip::copy(mcp_exchange::pretty(&message), "request", cx),
            None => crate::clip::announce_nothing("Nothing selected to send", cx),
        }
    }

    /// Copy the current request as a `curl` command.
    pub fn copy_request_curl(&mut self, cx: &mut Context<Self>) {
        match self.request_curl(cx) {
            Some(text) => crate::clip::copy(text, "curl command", cx),
            None => crate::clip::announce_nothing("A curl command needs an HTTP server", cx),
        }
    }

    /// Copy the client configuration, naming any credential left out of it.
    pub fn copy_server_config(&mut self, cx: &mut Context<Self>) {
        let config = self.client_config(cx);
        let note = if config.notes.is_empty() {
            "config".to_owned()
        } else {
            format!("Config · {} credential left out", config.notes.len())
        };
        crate::clip::copy(config.text(), note, cx);
    }

    /// Copy the selected server's bearer token. A keyring can block (a locked
    /// keychain asks the user), so it is read on the blocking pool; a model
    /// without a bridge reads it in place.
    pub fn copy_bearer_token(&mut self, cx: &mut Context<Self>) {
        let (key, secrets, bridge) = {
            let state = self.state.read(cx);
            let key = state
                .server()
                .and_then(|s| crate::state::keyring_id(&s.record.spec));
            (key, state.secrets.clone(), state.bridge.clone())
        };
        let (Some(key), Some(bridge)) = (key, bridge) else {
            return copy_token(self.bearer_token(cx), cx);
        };
        let read = bridge.run_blocking(move || secrets.get(&key).ok().flatten());
        cx.spawn(async move |_, cx| {
            let token = read.await.flatten();
            cx.update(|cx| copy_token(token, cx));
        })
        .detach();
    }

    /// Ask for a path and write one of the exports there once it is ready
    /// ([`Self::export_ready`]).
    pub fn export_file(&mut self, kind: Export, cx: &mut Context<Self>) {
        let mut export = self.export_ready(kind, cx);
        match export.try_recv() {
            Ok(Some(export)) => save_export(kind, export, cx),
            Ok(None) => cx
                .spawn(async move |_, cx| {
                    // Dropped when the server was deleted first: nothing to write.
                    if let Ok(export) = export.await {
                        cx.update(|cx| save_export(kind, export, cx));
                    }
                })
                .detach(),
            Err(_) => {}
        }
    }
}

/// Offer the save panel for an export, or say there is nothing to write.
pub(super) fn save_export(kind: Export, export: Option<(String, Vec<u8>)>, cx: &mut App) {
    match export {
        Some((name, contents)) => crate::clip::save(name, contents, cx),
        None => crate::clip::announce_nothing(format!("No {} to export", kind.label()), cx),
    }
}

/// Copy a bearer token read from the keyring, or say there is none.
pub(super) fn copy_token(token: Option<String>, cx: &mut App) {
    match token {
        Some(token) => crate::clip::copy(token, "bearer token", cx),
        None => crate::clip::announce_nothing("No token for this server", cx),
    }
}
