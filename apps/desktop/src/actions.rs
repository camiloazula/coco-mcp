//! Application actions and their default key bindings.
//!
//! Contexts: `Workspace` wraps the whole window; `ServerList` and `ItemList`
//! are the two focusable lists, so the arrow keys only move a list when it has
//! focus and never steal keystrokes from an input.

use gpui_kit::{Action, App, KeyBinding};
use serde::Deserialize;

gpui_kit::actions!(
    coco,
    [
        /// Open the "Add server" form (⌘N).
        AddServer,
        /// Run the current tool call or connect the form (⌘⏎).
        Call,
        /// Stop the request under the current selection (⌘.).
        CancelCall,
        /// Close the current form or panel (esc).
        Cancel,
        /// Answer the open dialog with its primary action (⏎, ⌘⏎).
        DialogAccept,
        /// Show or hide the log drawer (⌘J).
        ToggleLog,
        /// Clear the log drawer.
        ClearLog,
        /// Open the command palette (⌘K).
        CommandPalette,
        /// Switch between dark and light theme.
        ToggleTheme,
        /// Move selection up in the focused list.
        MoveUp,
        /// Move selection down in the focused list.
        MoveDown,
        /// Open the selected list item.
        Open,
        /// Show tools for the selected server.
        ShowTools,
        /// Show resources for the selected server.
        ShowResources,
        /// Show prompts for the selected server.
        ShowPrompts,
        /// Show the call history of the selected server.
        ShowHistory,
        /// Show what the selected server said about itself, and its roots.
        ShowServer,
        /// Connect or reconnect the selected server.
        Reconnect,
        /// Close the selected server's session.
        Disconnect,
        /// Ask to delete the selected server (⌘⌫).
        DeleteServer,
        /// Edit the selected server (⌘E).
        EditServer,
        /// Ask to delete the selected server's recorded calls.
        ClearHistory,
        /// Ask to remove the selected server's stored credentials.
        ForgetCredentials,
        /// Compare the selected server's snapshot with a snapshot file.
        CompareSnapshotFile,
        /// Copy the whole response under the current selection (⌘⇧C).
        CopyResponse,
        /// Copy the current request as a JSON-RPC message.
        CopyRequest,
        /// Copy the current request as a `curl` command (HTTP servers).
        CopyRequestCurl,
        /// Copy the selected server as an `mcpServers` block.
        CopyServerConfig,
        /// Copy the selected server's bearer token from the keyring.
        CopyBearerToken,
        /// Add servers from a client configuration file.
        ImportConfig,
        /// Write the last snapshot to a file.
        ExportSnapshot,
        /// Write the session's wire messages to a file.
        ExportLog,
        /// Write the recorded calls to a file.
        ExportHistory,
        /// Write the `mcpServers` block to a file.
        ExportConfig,
        /// Open the About window (application menu).
        About,
        /// Quit the application.
        Quit,
    ]
);

/// Select the server at `index` in the sidebar (dispatched by the palette).
#[derive(Debug, Action, Clone, PartialEq, Deserialize)]
#[action(namespace = coco, no_json)]
pub struct SelectServer {
    /// Sidebar position.
    pub index: usize,
}

/// Key context of the root view.
pub const WORKSPACE: &str = "Workspace";
/// Key context of the server list in the sidebar.
pub const SERVER_LIST: &str = "ServerList";
/// Key context of the middle list.
pub const ITEM_LIST: &str = "ItemList";
/// Key context of an open dialog, which takes the keyboard from the window.
pub const DIALOG: &str = "Dialog";

/// The window's shortcuts, inert while a dialog is open.
const WINDOW_SHORTCUTS: [&str; 15] = [
    "cmd-n",
    "cmd-.",
    "cmd-j",
    "cmd-k",
    "cmd-1",
    "cmd-2",
    "cmd-3",
    "cmd-4",
    "cmd-5",
    "cmd-t",
    "cmd-r",
    "cmd-shift-r",
    "cmd-backspace",
    "cmd-shift-c",
    "cmd-e",
];

/// Install the default bindings.
pub fn bind(cx: &mut App) {
    let list_bindings = |context: &'static str| {
        [
            KeyBinding::new("up", MoveUp, Some(context)),
            KeyBinding::new("down", MoveDown, Some(context)),
            KeyBinding::new("enter", Open, Some(context)),
        ]
    };
    cx.bind_keys(
        [
            KeyBinding::new("cmd-n", AddServer, Some(WORKSPACE)),
            KeyBinding::new("cmd-enter", Call, Some(WORKSPACE)),
            KeyBinding::new("cmd-.", CancelCall, Some(WORKSPACE)),
            KeyBinding::new("escape", Cancel, Some(WORKSPACE)),
            KeyBinding::new("cmd-j", ToggleLog, Some(WORKSPACE)),
            KeyBinding::new("cmd-k", CommandPalette, Some(WORKSPACE)),
            KeyBinding::new("cmd-1", ShowTools, Some(WORKSPACE)),
            KeyBinding::new("cmd-2", ShowResources, Some(WORKSPACE)),
            KeyBinding::new("cmd-3", ShowPrompts, Some(WORKSPACE)),
            KeyBinding::new("cmd-4", ShowHistory, Some(WORKSPACE)),
            KeyBinding::new("cmd-5", ShowServer, Some(WORKSPACE)),
            KeyBinding::new("cmd-t", ToggleTheme, Some(WORKSPACE)),
            KeyBinding::new("cmd-r", Reconnect, Some(WORKSPACE)),
            KeyBinding::new("cmd-shift-r", Disconnect, Some(WORKSPACE)),
            KeyBinding::new("cmd-backspace", DeleteServer, Some(WORKSPACE)),
            KeyBinding::new("cmd-e", EditServer, Some(WORKSPACE)),
            KeyBinding::new("cmd-shift-c", CopyResponse, Some(WORKSPACE)),
            KeyBinding::new("cmd-q", Quit, None),
        ]
        .into_iter()
        .chain(list_bindings(SERVER_LIST))
        .chain(list_bindings(ITEM_LIST))
        .chain(dialog_bindings()),
    );
}

/// While a dialog is open, Enter and ⌘⏎ answer it, Esc cancels it, and every
/// other window shortcut does nothing: a key must not send a request to, or
/// reconnect, a server that is waiting for the dialog's answer. An input in
/// the dialog keeps its own Enter.
fn dialog_bindings() -> Vec<KeyBinding> {
    let mut bindings = vec![
        KeyBinding::new("enter", DialogAccept, Some(DIALOG)),
        KeyBinding::new("cmd-enter", DialogAccept, Some(DIALOG)),
        KeyBinding::new("escape", Cancel, Some(DIALOG)),
    ];
    bindings.extend(
        WINDOW_SHORTCUTS
            .iter()
            .map(|keys| KeyBinding::new(keys, gpui_kit::NoAction {}, Some(DIALOG))),
    );
    bindings
}
