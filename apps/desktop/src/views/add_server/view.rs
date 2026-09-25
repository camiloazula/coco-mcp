//! Drawing the add-server form.

use gpui_kit::component::tooltip::Tooltip;

use super::*;

/// What choosing `mode` does, beside its name in the Protocol menu.
pub(super) fn protocol_note(mode: ProtocolMode) -> &'static str {
    match mode {
        ProtocolMode::Legacy => "Initialize handshake, 2025-11-25 or older.",
        ProtocolMode::Auto => "2026-07-28 when the server has it, else the handshake.",
        ProtocolMode::Modern => "2026-07-28 only. Fails on older servers.",
    }
}

/// Why Authorize waits while the fields differ from the saved settings.
const UNSAVED_AUTHORIZE: &str =
    "Connect saves the changes first; Authorize signs in with the saved settings";

impl AddServerForm {
    /// The server whose failed connect this form writes under its fields
    /// now; `None` while it shows its own error instead, or no failure.
    pub fn failure_shown(&self, cx: &App) -> Option<usize> {
        let ix = self.edited(cx)?;
        let failed = matches!(self.state.read(cx).servers[ix].status, Status::Error(_));
        (failed && self.error.is_none()).then_some(ix)
    }
}

impl Render for AddServerForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *tokens(cx);
        let state = self.state.clone();
        let editing = self.is_editing();
        // Opened on purpose, the form can be left; shown because the server
        // is off, it is the pane, and there is nothing to go back to.
        let opened = self.state.read(cx).screen == Screen::AddServer;
        // How connecting with the saved settings is going, under the fields:
        // the form stays until it succeeds. The auth is the saved spec's, not
        // the one the form opened with: a save that changed it is what the
        // last connect tried.
        let (status, refused, oauth) = self
            .edited(cx)
            .and_then(|ix| self.state.read(cx).servers.get(ix))
            .map_or((None, false, false), |s| {
                let oauth = matches!(
                    s.record.spec,
                    ServerSpec::Http {
                        auth: AuthRef::OAuth { .. },
                        ..
                    }
                );
                (Some(s.status.clone()), s.unauthorized, oauth)
            });
        let connecting = status == Some(Status::Connecting);
        // Saving the settings of a server in session ends that session and
        // starts another: the button says so rather than offering a connect.
        // Read from the server as it is now, which can change under the
        // form (the row's plug, a connect finishing).
        let live = status == Some(Status::Connected);
        let failed = match status {
            Some(Status::Error(text)) => Some(text),
            _ => None,
        };
        // Turned away for want of credentials, the server needs them set
        // here; a refused OAuth login is fixed by logging in again instead,
        // and that is the one button the failure adds.
        let unauthorized = failed.is_some() && refused;
        let reauthorize = unauthorized && oauth;
        let as_saved = reauthorize && self.unchanged(cx);
        let (title, subtitle) = match (editing, opened) {
            (true, true) if failed.is_some() => (
                "Edit server",
                "The connection failed. Change the settings and connect again.",
            ),
            (true, false) if reauthorize => (
                "Authorization required",
                "Authorize to sign in again, or change the settings first.",
            ),
            (true, false) if unauthorized => (
                "Authorization required",
                "Set the credentials and connect again.",
            ),
            (true, false) if failed.is_some() => (
                "Connection failed",
                "Change the settings and connect again.",
            ),
            (true, true) if live => (
                "Edit server",
                "Saving ends the session and connects with the new settings.",
            ),
            (true, true) => ("Edit server", "Saving connects with the new settings."),
            (true, false) => (
                "Disconnected",
                "Connect with these settings, or change them first.",
            ),
            (false, _) => (
                "Add server",
                "Saved to the sidebar. Connection is attempted immediately.",
            ),
        };
        let action = if editing && opened && live {
            "Save & reconnect"
        } else {
            "Connect"
        };
        let mono_family = cx.theme().mono_font_family.clone();
        let mono_input = |input: Input| input.font_family(mono_family.clone()).text_size(px(12.));
        let header = h_flex()
            .items_baseline()
            .justify_between()
            .pt(px(16.))
            .px(px(24.))
            .pb(px(12.))
            .child(
                div()
                    .text_size(px(15.))
                    .font_weight(FontWeight::MEDIUM)
                    .child(title),
            )
            .when(opened, |el| {
                el.child(
                    h_flex()
                        .id("cancel-add")
                        .gap(px(4.))
                        .text_size(px(12.))
                        .text_color(t.muted)
                        .cursor_pointer()
                        .on_click(move |_, _, cx| {
                            state.update(cx, |s, cx| s.cancel_add_server(cx));
                        })
                        .child("Cancel")
                        .child(kbd(cx, "Esc")),
                )
            });
        let transport = h_flex()
            .gap(px(16.))
            .child(
                text_tab(cx, "Stdio", self.stdio)
                    .id("transport-stdio")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.stdio = true;
                        cx.notify();
                    }))
                    .test_support(),
            )
            .child(
                text_tab(cx, "HTTP", !self.stdio)
                    .id("transport-http")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.stdio = false;
                        cx.notify();
                    }))
                    .test_support(),
            );
        // The closed field names the era; the open menu explains each one.
        // Sized like the inputs beside it: their default type and padding,
        // at their 26px height.
        let protocol = div()
            .id("protocol")
            .w(px(160.))
            .child(
                Select::new(&self.protocol_select)
                    .h(px(26.))
                    .menu_width(px(440.)),
            )
            .test_support();
        let mut grid = v_flex()
            .px(px(24.))
            .py(px(16.))
            .gap(px(10.))
            .max_w(px(640.))
            .border_t_1()
            .border_color(t.hair)
            .child(self.field(
                cx,
                "Name",
                false,
                Input::new(&self.name).id("name").h(px(26.)),
            ))
            .child(self.field(cx, "Transport", false, transport))
            .child(self.field(cx, "Protocol", false, protocol));
        if self.stdio {
            grid = grid
                .child(self.field(
                    cx,
                    "Command",
                    false,
                    mono_input(Input::new(&self.command).id("command").h(px(26.))),
                ))
                .child(self.field(
                    cx,
                    "Working directory",
                    false,
                    mono_input(Input::new(&self.cwd).id("cwd").h(px(26.))),
                ))
                .child(self.field(cx, "Environment", true, Textarea::new(&self.env)));
        } else {
            let auth_tabs = h_flex()
                .gap(px(16.))
                .child(
                    text_tab(cx, "None", self.auth == AuthKind::None)
                        .id("auth-none")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.auth = AuthKind::None;
                            cx.notify();
                        }))
                        .test_support(),
                )
                .child(
                    text_tab(cx, "Bearer", self.auth == AuthKind::Bearer)
                        .id("auth-bearer")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.auth = AuthKind::Bearer;
                            cx.notify();
                        }))
                        .test_support(),
                )
                .child(
                    text_tab(cx, "OAuth", self.auth == AuthKind::OAuth)
                        .id("auth-oauth")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.auth = AuthKind::OAuth;
                            cx.notify();
                        }))
                        .test_support(),
                );
            grid = grid
                .child(self.field(
                    cx,
                    "URL",
                    false,
                    mono_input(Input::new(&self.url).id("url").h(px(26.))),
                ))
                .child(self.field(cx, "Headers", true, Textarea::new(&self.headers)))
                .child(self.field(cx, "Auth", false, auth_tabs));
            if self.auth == AuthKind::Bearer {
                grid = grid.child(self.field(
                    cx,
                    "Token",
                    false,
                    mono_input(Input::new(&self.token).id("token").mask_toggle().h(px(26.))),
                ));
            }
        }
        grid = grid.child(
            h_flex()
                .gap(px(16.))
                .child(div().w(px(140.)).flex_none())
                .child(
                    h_flex()
                        .gap(px(10.))
                        .pt(px(6.))
                        // Authorize signs in with the saved settings; with
                        // changes in the fields it waits for Connect to save
                        // them, rather than dropping them.
                        .when(reauthorize, |row| {
                            let button = accent_button(cx, "Authorize", 26.)
                                .id("authorize")
                                .aria_label("Authorize");
                            row.child(if as_saved {
                                button
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if let Some(ix) = this.edited(cx) {
                                            this.state.update(cx, |s, cx| s.authorize(ix, cx));
                                        }
                                    }))
                                    .test_support()
                            } else {
                                button
                                    .opacity(0.5)
                                    .cursor_default()
                                    .tooltip(|window, cx| {
                                        Tooltip::new(UNSAVED_AUTHORIZE).build(window, cx)
                                    })
                                    .on_click(|_, _, cx| {
                                        crate::clip::announce_nothing(UNSAVED_AUTHORIZE, cx)
                                    })
                                    .test_support()
                            })
                        })
                        .child(
                            accent_button(cx, action, 26.)
                                .when(reauthorize && as_saved, |el| el.bg(t.sunk).text_color(t.fg))
                                .id("connect")
                                .aria_label(action)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.submit(cx);
                                }))
                                .test_support(),
                        )
                        .child(kbd(cx, "⌘⏎"))
                        .children(self.error.clone().map(|e| {
                            div()
                                .id("form-error")
                                .text_size(px(12.))
                                .text_color(t.err)
                                .child(e)
                                .test_support()
                        }))
                        .when(self.error.is_none() && connecting, |row| {
                            row.child(
                                muted(cx, 12., "Connecting…")
                                    .id("form-connecting")
                                    .test_support(),
                            )
                        }),
                ),
        );
        // The failure of the last connect, on a line of its own under the
        // button, wrapped to the form's width.
        if let Some(text) = failed.filter(|_| self.error.is_none() && !connecting) {
            grid = grid.child(
                h_flex()
                    .gap(px(16.))
                    .child(div().w(px(140.)).flex_none())
                    .child(
                        div()
                            .id("connect-error")
                            .flex_1()
                            .min_w_0()
                            .text_size(px(12.))
                            .text_color(t.err)
                            .whitespace_normal()
                            .child(text)
                            .test_support(),
                    ),
            );
        }
        v_flex()
            .id("add-server-form")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .child(header)
            .child(
                muted(cx, 13., subtitle)
                    .px(px(24.))
                    .pb(px(16.))
                    .max_w(px(640.)),
            )
            .child(grid)
    }
}
