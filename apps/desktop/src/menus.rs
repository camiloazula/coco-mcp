//! The native menu bar and the global action handlers behind it.
//!
//! Menu items dispatch GPUI actions to the active window first and then to
//! the global listeners registered here, so `About` and `Quit` work whichever
//! window is in front. The Edit menu maps the standard OS actions onto
//! gpui-kit's input actions, which gives the text fields their menu-driven
//! Cut / Copy / Paste on macOS.

use gpui_kit::component::Root;
use gpui_kit::component::input::{Copy, Cut, Paste, Redo, SelectAll, Undo};
use gpui_kit::{App, Global, Menu, MenuItem, OsAction, WindowHandle};
use mcp_core::{AuthRef, ServerSpec};

use crate::APP_NAME;
use crate::actions::{
    About, AddServer, CancelCall, ClearHistory, CommandPalette, CompareSnapshotFile,
    CopyBearerToken, CopyRequest, CopyRequestCurl, CopyResponse, CopyServerConfig, DeleteServer,
    Disconnect, EditServer, ExportConfig, ExportHistory, ExportLog, ExportSnapshot,
    ForgetCredentials, ImportConfig, Quit, Reconnect, ShowHistory, ShowPrompts, ShowResources,
    ShowServer, ShowTools, ToggleLog, ToggleResponse, ToggleTheme, ZoomLog,
};
use crate::state::{AppState, Status};
use crate::views::about;

/// The About window, while one is open.
#[derive(Debug, Default)]
pub struct AboutWindow(Option<WindowHandle<Root>>);

impl Global for AboutWindow {}

/// Open the About window, or bring the existing one to the front.
pub fn open_about(cx: &mut App) {
    let existing = cx.try_global::<AboutWindow>().and_then(|w| w.0);
    if let Some(handle) = about::open(existing.as_ref(), cx) {
        cx.global_mut::<AboutWindow>().0 = Some(handle);
    }
}

/// What the selected server can do now, which decides the menu items that
/// are enabled; the palette hides the same entries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Available {
    server: bool,
    connected: bool,
    pending: bool,
    http: bool,
    bearer: bool,
    credentials: bool,
    response: bool,
    snapshot: bool,
}

impl Available {
    /// Read it off the model.
    pub fn of(state: &AppState) -> Self {
        let server = state.server();
        let spec = server.map(|s| &s.record.spec);
        Self {
            server: server.is_some(),
            connected: server.is_some_and(|s| s.status == Status::Connected),
            pending: state.response_pending(),
            http: matches!(spec, Some(ServerSpec::Http { .. })),
            bearer: matches!(
                spec,
                Some(ServerSpec::Http {
                    auth: AuthRef::Bearer { .. },
                    ..
                })
            ),
            credentials: spec.is_some_and(|s| crate::state::keyring_id(s).is_some()),
            response: state.response().is_some(),
            snapshot: state.snapshot().is_some(),
        }
    }
}

/// What the menu bar was last installed for, so it is rebuilt only when that
/// changes rather than on every change to the model.
#[derive(Debug, Default)]
struct Installed(Option<Available>);

impl Global for Installed {}

/// Register the global handlers and install the menu bar.
pub fn install(cx: &mut App) {
    cx.set_global(AboutWindow::default());
    cx.on_action(|_: &About, cx| open_about(cx));
    cx.on_action(|_: &Quit, cx| cx.quit());
    refresh(Available::default(), cx);
}

/// Install the menu bar with the items `available` allows, unless it already
/// is.
pub fn refresh(available: Available, cx: &mut App) {
    if cx
        .try_global::<Installed>()
        .is_some_and(|installed| installed.0 == Some(available))
    {
        return;
    }
    cx.set_menus(menu_bar(available));
    cx.set_global(Installed(Some(available)));
}

fn menu_bar(a: Available) -> Vec<Menu> {
    let menu = |name: &'static str, items: Vec<MenuItem>| Menu {
        name: name.into(),
        items,
        disabled: false,
    };
    vec![
        menu(
            APP_NAME,
            vec![
                MenuItem::action(format!("About {APP_NAME}"), About),
                MenuItem::separator(),
                MenuItem::action(format!("Quit {APP_NAME}"), Quit),
            ],
        ),
        // Everything a debugger is used for ends in taking data out, so
        // the exports get a File menu of their own rather than hiding under
        // the palette.
        menu(
            "File",
            vec![
                MenuItem::action("Import Servers…", ImportConfig),
                MenuItem::separator(),
                MenuItem::action("Export Snapshot…", ExportSnapshot).disabled(!a.snapshot),
                MenuItem::action("Export Log…", ExportLog).disabled(!a.server),
                MenuItem::action("Export History…", ExportHistory).disabled(!a.server),
                MenuItem::separator(),
                MenuItem::action("Export Server Config…", ExportConfig).disabled(!a.server),
                MenuItem::separator(),
                MenuItem::action("Compare with Snapshot File…", CompareSnapshotFile)
                    .disabled(!a.snapshot),
            ],
        ),
        menu(
            "Edit",
            vec![
                MenuItem::os_action("Undo", Undo, OsAction::Undo),
                MenuItem::os_action("Redo", Redo, OsAction::Redo),
                MenuItem::separator(),
                MenuItem::os_action("Cut", Cut, OsAction::Cut),
                MenuItem::os_action("Copy", Copy, OsAction::Copy),
                MenuItem::os_action("Paste", Paste, OsAction::Paste),
                MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
                MenuItem::separator(),
                MenuItem::action("Copy Response", CopyResponse).disabled(!a.response),
                MenuItem::action("Copy Request as JSON-RPC", CopyRequest).disabled(!a.server),
                MenuItem::action("Copy Request as curl", CopyRequestCurl).disabled(!a.http),
            ],
        ),
        menu(
            "Server",
            vec![
                MenuItem::action("Add Server…", AddServer),
                MenuItem::action("Edit Server…", EditServer).disabled(!a.server),
                MenuItem::separator(),
                MenuItem::action("Reconnect", Reconnect).disabled(!a.server),
                MenuItem::action("Disconnect", Disconnect).disabled(!a.connected),
                MenuItem::action("Cancel Request", CancelCall).disabled(!a.pending),
                MenuItem::separator(),
                MenuItem::action("Copy Server Config", CopyServerConfig).disabled(!a.server),
                // Named for what it does: everything else redacts the token.
                MenuItem::action("Copy Bearer Token", CopyBearerToken).disabled(!a.bearer),
                MenuItem::action("Forget Credentials…", ForgetCredentials).disabled(!a.credentials),
                MenuItem::separator(),
                MenuItem::action("Clear History…", ClearHistory).disabled(!a.server),
                MenuItem::action("Delete Server…", DeleteServer).disabled(!a.server),
            ],
        ),
        menu(
            "View",
            vec![
                MenuItem::action("Tools", ShowTools),
                MenuItem::action("Resources", ShowResources),
                MenuItem::action("Prompts", ShowPrompts),
                MenuItem::action("History", ShowHistory),
                MenuItem::action("Server", ShowServer),
                MenuItem::separator(),
                MenuItem::action("Toggle Log", ToggleLog),
                MenuItem::action("Zoom Log", ZoomLog),
                MenuItem::action("Toggle Response", ToggleResponse),
                MenuItem::action("Toggle Theme", ToggleTheme),
                MenuItem::action("Command Palette", CommandPalette),
            ],
        ),
        // GPUI hands a menu named "Window" to the OS as the windows menu.
        menu("Window", vec![]),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disabled(items: &[MenuItem], name: &str) -> bool {
        items
            .iter()
            .find_map(|item| match item {
                MenuItem::Action {
                    name: n, disabled, ..
                } if n.as_ref() == name => Some(*disabled),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no item {name}"))
    }

    #[test]
    fn what_cannot_run_is_disabled() {
        let mut state = AppState::new(None, None);
        let none = menu_bar(Available::of(&state));
        assert!(disabled(&none[3].items, "Delete Server…"), "no server");
        assert!(!disabled(&none[3].items, "Add Server…"));
        let spec = ServerSpec::Http {
            url: "https://remote.test/mcp".into(),
            headers: Default::default(),
            auth: AuthRef::Bearer {
                keyring_id: "k".into(),
            },
        };
        state.add_demo_server("remote", spec, Status::Off);
        let off = menu_bar(Available::of(&state));
        let server = &off[3].items;
        assert!(!disabled(server, "Delete Server…"));
        assert!(!disabled(server, "Copy Bearer Token"));
        assert!(!disabled(server, "Forget Credentials…"));
        assert!(disabled(server, "Disconnect"), "not connected");
        assert!(disabled(server, "Cancel Request"), "nothing pending");
        assert!(!disabled(&off[2].items, "Copy Request as curl"));
    }
}
