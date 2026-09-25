//! Left column: servers and the Tools / Resources / Prompts switch.
//! Sidebar 200px, `sunk`, rows 28px, padding 0 12px.

use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, FocusHandle, FontWeight, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, TestSupportExt as _, Window, div, px,
};

use crate::actions::{self, MoveDown, MoveUp, Open, SERVER_LIST};
use crate::clip;
use crate::state::{Mode, Status};
use crate::theme::tokens;
use crate::views::{Workspace, mono};

/// Render the sidebar. `focus` is the server list's focus handle.
pub fn render(
    ws: &Workspace,
    focus: &FocusHandle,
    _window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let theme = *tokens(cx);
    let state = ws.state.read(cx);
    let selected_server = state.selected_server;
    let entity = ws.state.clone();
    let is_form = state.screen == crate::state::Screen::AddServer;

    let has_selection = selected_server.is_some() && !is_form;
    // A server the pane already shows as its settings has nothing to open.
    let editable = has_selection && state.settings_pane().is_none();
    // Header actions: add, edit, delete. Each is a Lucide icon with a hover
    // caption; delete needs a selected server, edit one whose settings are
    // not on screen already.
    let action = |id: &'static str,
                  icon: gpui_kit::assets::IconName,
                  caption: &'static str,
                  hover_color: gpui_kit::Hsla| {
        div()
            .id(id)
            .p(px(2.))
            .text_color(theme.muted)
            .hover(move |s| s.text_color(hover_color))
            .cursor_pointer()
            .tooltip(move |window, cx| Tooltip::new(caption).build(window, cx))
            .child(Icon::new(icon).with_size(px(12.)).text_color(theme.muted))
    };
    let header = h_flex()
        .justify_between()
        .items_center()
        .px(px(12.))
        .pb(px(6.))
        .text_size(px(11.))
        .text_color(theme.muted)
        .child("SERVERS")
        .child(div().flex_1())
        .child(
            h_flex()
                .gap(px(8.))
                .child(
                    action(
                        "add-server",
                        gpui_kit::assets::IconName::Plus,
                        "Add server",
                        theme.fg,
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_add_server(window, cx);
                    }))
                    .test_support(),
                )
                .when(editable, |el| {
                    el.child(
                        action(
                            "edit-server",
                            gpui_kit::assets::IconName::Pencil,
                            "Edit server",
                            theme.fg,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.state.update(cx, |s, cx| s.show_edit_selected(cx));
                        }))
                        .test_support(),
                    )
                })
                .when(has_selection, |el| {
                    el.child(
                        action(
                            "delete-server",
                            gpui_kit::assets::IconName::Trash,
                            "Delete server",
                            theme.err,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.state.update(cx, |s, cx| s.request_delete_selected(cx));
                        }))
                        .test_support(),
                    )
                }),
        );

    let servers: Vec<AnyElement> = state
        .servers
        .iter()
        .enumerate()
        .map(|(ix, entry)| {
            let selected = selected_server == Some(ix) && !is_form;
            // Connecting is a ring in the connected colour: on its way, not off.
            let connecting = entry.status == Status::Connecting;
            let dot = match entry.status {
                Status::Connected | Status::Connecting => theme.accent,
                Status::Error(_) => theme.err,
                Status::Off => theme.hair,
            };
            let entity = entity.clone();
            let plug = entity.clone();
            // The row's own record, so a right-click on a server that is not
            // the selected one still copies that server.
            let config = mcp_exchange::client_config(std::slice::from_ref(&entry.record)).text();
            h_flex()
                .id(("server", ix))
                .h(px(28.))
                .px(px(12.))
                .gap(px(10.))
                .text_color(if selected_server == Some(ix) {
                    theme.fg
                } else {
                    theme.muted
                })
                .font_weight(if selected_server == Some(ix) {
                    FontWeight::MEDIUM
                } else {
                    FontWeight::NORMAL
                })
                .when(selected, |el| el.bg(theme.sel))
                .hover(|s| s.bg(theme.hover))
                // A click selects the server; the second click of a double
                // click connects or disconnects it.
                .on_click(move |event, _, cx| {
                    entity.update(cx, |state, cx| {
                        state.select_server(ix, cx);
                        if event.click_count() == 2 {
                            state.toggle_connection(ix, cx);
                        }
                    });
                })
                .child(
                    div()
                        .flex_none()
                        .size(px(7.))
                        .rounded_full()
                        .when(connecting, |el| el.border_1().border_color(dot))
                        .when(!connecting, |el| el.bg(dot)),
                )
                .child(
                    mono(cx, 12., entry.record.name.clone())
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis(),
                )
                // The plug at the row's end connects an off or failed server
                // and disconnects a running one, without the double click.
                .child({
                    let (icon, caption) = match entry.status {
                        Status::Connected => (gpui_kit::assets::IconName::Unplug, "Disconnect"),
                        Status::Connecting => (gpui_kit::assets::IconName::Unplug, "Connecting…"),
                        Status::Off | Status::Error(_) => {
                            (gpui_kit::assets::IconName::Plug, "Connect")
                        }
                    };
                    div()
                        .id(("server-plug", ix))
                        .flex_none()
                        .p(px(2.))
                        .text_color(theme.muted)
                        .hover(move |s| s.text_color(theme.fg))
                        .cursor_pointer()
                        .tooltip(move |window, cx| Tooltip::new(caption).build(window, cx))
                        .on_click(move |_, _, cx| {
                            cx.stop_propagation();
                            plug.update(cx, |state, cx| state.toggle_connection(ix, cx));
                        })
                        .child(Icon::new(icon).with_size(px(12.)))
                        .test_support()
                })
                .test_support()
                .on_mouse_down(gpui_kit::MouseButton::Right, move |event, _, cx| {
                    clip::open_menu(
                        vec![clip::MenuEntry::new(
                            "Copy server config",
                            "config",
                            config.clone(),
                        )],
                        event.position,
                        cx,
                    );
                })
                .into_any_element()
        })
        .collect();

    let modes: Vec<AnyElement> = Mode::ALL
        .iter()
        .enumerate()
        .map(|(mode_ix, &mode)| {
            let active = state.mode == mode && !is_form;
            let count = state.count(mode).map(|n| n.to_string()).unwrap_or_default();
            // A count that is low because a list failed must not read as the truth.
            let count_color = if state.list_failures(mode).is_empty() {
                theme.muted
            } else {
                theme.err
            };
            let entity = entity.clone();
            h_flex()
                .id(("mode", mode_ix))
                .h(px(28.))
                .px(px(12.))
                .justify_between()
                .text_color(if active { theme.fg } else { theme.muted })
                .font_weight(if active {
                    FontWeight::MEDIUM
                } else {
                    FontWeight::NORMAL
                })
                .hover(|s| s.bg(theme.hover))
                .on_click(move |_, _, cx| {
                    entity.update(cx, |state, cx| state.set_mode(mode, cx));
                })
                .child(div().text_size(px(13.)).child(mode.label()))
                .child(mono(cx, 11., count).text_color(count_color))
                .test_support()
                .into_any_element()
        })
        .collect();

    v_flex()
        .id("sidebar")
        .key_context(SERVER_LIST)
        .track_focus(focus)
        .on_action(cx.listener(|this, _: &MoveUp, _, cx| {
            this.state.update(cx, |s, cx| s.move_server(-1, cx));
        }))
        .on_action(cx.listener(|this, _: &MoveDown, _, cx| {
            this.state.update(cx, |s, cx| s.move_server(1, cx));
        }))
        .on_action(cx.listener(|this, _: &Open, _, cx| {
            this.state.update(cx, |s, cx| {
                if let Some(ix) = s.selected_server
                    && matches!(s.servers[ix].status, Status::Off | Status::Error(_))
                {
                    s.connect(ix, cx);
                }
            });
        }))
        .on_action(cx.listener(|this, _: &actions::Reconnect, _, cx| {
            this.state.update(cx, |s, cx| {
                if let Some(ix) = s.selected_server {
                    s.connect(ix, cx);
                }
            });
        }))
        .h_full()
        .w_full()
        .py(px(12.))
        .bg(theme.sunk)
        .child(header)
        .when(servers.is_empty(), |el| {
            el.child(
                div()
                    .px(px(12.))
                    .py(px(6.))
                    .text_size(px(12.))
                    .text_color(theme.muted)
                    .child("No servers"),
            )
        })
        // The servers scroll; the mode switch stays at the bottom.
        .child(
            v_flex()
                .id("server-rows")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&ws.server_scroll)
                .children(servers)
                .test_support(),
        )
        .children(modes)
        .into_any_element()
}
