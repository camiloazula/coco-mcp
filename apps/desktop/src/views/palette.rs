//! Command palette (⌘K) on gpui-component's `Command`: servers, views and
//! actions, filtered as you type. Confirming dispatches the item's action
//! through the workspace's handlers, so every palette entry is also a key
//! binding, and the hint column shows it.

use gpui_kit::component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_kit::{
    Action, AnyElement, AppContext as _, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, TestSupportExt as _, Window, div, hsla, px,
};

use crate::actions::{
    AddServer, Call, CancelCall, ClearHistory, ClearLog, CompareSnapshotFile, CopyBearerToken,
    CopyRequest, CopyRequestCurl, CopyResponse, CopyServerConfig, DeleteServer, Disconnect,
    EditServer, ExportConfig, ExportHistory, ExportLog, ExportSnapshot, ForgetCredentials,
    ImportConfig, Reconnect, SelectServer, ShowHistory, ShowPrompts, ShowResources, ShowServer,
    ShowTools, ToggleLog, ToggleTheme,
};
use crate::state::{Mode, Status};
use crate::views::Workspace;

impl Workspace {
    /// Open the palette (creating its state on first use) and focus its query.
    pub fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = match &self.palette {
            Some(state) => state.clone(),
            None => {
                let state = cx.new(|cx| CommandState::new(window, cx));
                self.palette = Some(state.clone());
                state
            }
        };
        self.palette_open = true;
        state.update(cx, |s, cx| s.set_query("", window, cx));
        // The palette is created inside this render pass; focusing must wait
        // until it has laid out once.
        window.defer(cx, move |window, cx| {
            state.update(cx, |s, cx| s.focus(window, cx));
        });
        cx.notify();
    }

    /// Close the palette and return focus to the list.
    pub fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette_open {
            self.palette_open = false;
            window.focus(&self.list_focus, cx);
            cx.notify();
        }
    }
}

fn item(label: &'static str, action: impl Action, keywords: &[&'static str]) -> CommandItem {
    CommandItem::new()
        .label(label)
        .keywords(keywords.iter().copied())
        .action(Box::new(action))
}

/// The palette overlay, when open.
pub fn render(
    ws: &mut Workspace,
    _window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Option<AnyElement> {
    if !ws.palette_open {
        return None;
    }
    let state = ws.palette.clone()?;
    let t = *crate::theme::tokens(cx);
    let (servers, selected, connected, mode, drawer_open, dark, spec) = {
        let s = ws.state.read(cx);
        (
            s.servers
                .iter()
                .map(|e| (e.record.name.clone(), e.status.clone()))
                .collect::<Vec<_>>(),
            s.selected_server,
            s.server().is_some_and(|e| e.status == Status::Connected),
            s.mode,
            s.drawer_open,
            s.dark,
            s.server().map(|e| e.record.spec.clone()),
        )
    };
    let http = matches!(spec, Some(mcp_core::ServerSpec::Http { .. }));
    let has_credentials = matches!(
        &spec,
        Some(mcp_core::ServerSpec::Http {
            auth: mcp_core::AuthRef::Bearer { .. } | mcp_core::AuthRef::OAuth { .. },
            ..
        })
    );
    let has_token = matches!(
        &spec,
        Some(mcp_core::ServerSpec::Http {
            auth: mcp_core::AuthRef::Bearer { .. },
            ..
        })
    );

    let mut server_items: Vec<CommandItem> = servers
        .iter()
        .enumerate()
        .map(|(index, (name, status))| {
            let dot = match status {
                Status::Connected => "connected",
                Status::Connecting => "connecting",
                Status::Off => "disconnected",
                Status::Error(_) => "error",
            };
            CommandItem::new()
                .label(format!("Switch to {name}"))
                .keywords([name.as_str(), dot])
                .checked(selected == Some(index))
                .action(Box::new(SelectServer { index }))
        })
        .collect();
    server_items.push(item("Add server", AddServer, &["new", "stdio", "http"]));
    server_items.push(item(
        "Import servers…",
        ImportConfig,
        &["config", "mcpservers", "servers", "file", "json"],
    ));
    if selected.is_some() {
        server_items.push(item(
            if connected { "Reconnect" } else { "Connect" },
            Reconnect,
            &["connect", "reconnect"],
        ));
        if connected {
            server_items.push(item("Disconnect", Disconnect, &["close"]));
        }
        server_items.push(item(
            "Edit server",
            EditServer,
            &["edit", "rename", "settings"],
        ));
        server_items.push(item(
            "Clear history…",
            ClearHistory,
            &["history", "calls", "delete", "forget"],
        ));
        if has_credentials {
            server_items.push(item(
                "Forget credentials…",
                ForgetCredentials,
                &["token", "oauth", "keyring", "sign out"],
            ));
        }
        server_items.push(item("Delete server…", DeleteServer, &["remove", "delete"]));
    }

    let view_items = vec![
        item("Show tools", ShowTools, &["tools"]).checked(mode == Mode::Tools),
        item("Show resources", ShowResources, &["resources"]).checked(mode == Mode::Resources),
        item("Show prompts", ShowPrompts, &["prompts"]).checked(mode == Mode::Prompts),
        item("Show history", ShowHistory, &["history", "calls", "replay"])
            .checked(mode == Mode::History),
        item(
            "Show server",
            ShowServer,
            &[
                "server",
                "capabilities",
                "instructions",
                "roots",
                "protocol",
            ],
        )
        .checked(mode == Mode::Server),
    ];
    let pending = ws.state.read(cx).response_pending();
    let mut action_items = vec![
        item("Call", Call, &["run", "send", "read", "get", "replay"]),
        item(
            if drawer_open { "Hide log" } else { "Show log" },
            ToggleLog,
            &["log", "drawer", "events"],
        ),
        item("Clear log", ClearLog, &["log"]),
        item(
            if dark { "Light theme" } else { "Dark theme" },
            ToggleTheme,
            &["theme", "dark", "light"],
        ),
    ];
    if pending {
        action_items.insert(
            1,
            item("Cancel request", CancelCall, &["stop", "abort", "pending"]),
        );
    }

    // Copying and exporting are the point of a debugger, so both are in
    // the palette as well as in the menu bar.
    let mut copy_items = vec![
        item("Copy response", CopyResponse, &["result", "json", "output"]),
        item(
            "Copy request as JSON-RPC",
            CopyRequest,
            &["jsonrpc", "message", "send"],
        ),
    ];
    if http {
        copy_items.push(item(
            "Copy request as curl",
            CopyRequestCurl,
            &["curl", "shell", "http", "reproduce"],
        ));
    }
    if selected.is_some() {
        copy_items.push(item(
            "Copy server config",
            CopyServerConfig,
            &["mcpservers", "servers", "clients", "json"],
        ));
    }
    if has_token {
        copy_items.push(item(
            "Copy bearer token",
            CopyBearerToken,
            &["secret", "keyring", "authorization"],
        ));
    }
    let mut export_items = vec![
        item(
            "Export snapshot…",
            ExportSnapshot,
            &["save", "file", "baseline", "diff"],
        ),
        item("Export log…", ExportLog, &["save", "file", "jsonl", "wire"]),
        item(
            "Export history…",
            ExportHistory,
            &["save", "file", "jsonl", "calls"],
        ),
    ];
    if selected.is_some() {
        export_items.push(item(
            "Compare with snapshot file…",
            CompareSnapshotFile,
            &["diff", "baseline", "file", "snapshot"],
        ));
        export_items.push(item(
            "Export server config…",
            ExportConfig,
            &["save", "file", "mcpservers"],
        ));
    }

    let close_confirm = cx.entity();
    let close_cancel = cx.entity();
    let card = div()
        .id("command-palette")
        .on_click(|_, _, cx| cx.stop_propagation())
        .w(px(560.))
        .bg(t.bg)
        .border_1()
        .border_color(t.hair)
        .rounded(px(3.))
        .overflow_hidden()
        .child(
            Command::new(&state)
                .bordered(false)
                .placeholder("Type a command…")
                .max_h(px(360.))
                .group(CommandGroup::new().label("Servers").items(server_items))
                .group(CommandGroup::new().label("Views").items(view_items))
                .group(CommandGroup::new().label("Actions").items(action_items))
                .group(CommandGroup::new().label("Copy").items(copy_items))
                .group(CommandGroup::new().label("Export").items(export_items))
                .on_confirm(move |_, window, cx| {
                    close_confirm.update(cx, |ws, cx| ws.close_palette(window, cx));
                })
                .on_cancel(move |window, cx| {
                    close_cancel.update(cx, |ws, cx| ws.close_palette(window, cx));
                }),
        )
        .test_support();
    let backdrop = cx.entity();
    Some(
        div()
            .id("palette-overlay")
            .occlude()
            .absolute()
            .inset_0()
            .flex()
            .items_start()
            .justify_center()
            .pt(px(96.))
            .bg(hsla(0., 0., 0., 0.4))
            .on_click(move |_, window, cx| {
                backdrop.update(cx, |ws, cx| ws.close_palette(window, cx));
            })
            .child(card)
            .into_any_element(),
    )
}
