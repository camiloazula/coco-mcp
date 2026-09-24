//! Drawing the add-server form.

use super::*;

/// What choosing `mode` does, beside its name in the Protocol menu.
pub(super) fn protocol_note(mode: ProtocolMode) -> &'static str {
    match mode {
        ProtocolMode::Legacy => "Initialize handshake, 2025-11-25 or older.",
        ProtocolMode::Auto => "2026-07-28 when the server has it, else the handshake.",
        ProtocolMode::Modern => "2026-07-28 only. Fails on older servers.",
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
        // the form stays until it succeeds.
        let status = self.editing.as_ref().and_then(|(ix, _)| {
            self.state
                .read(cx)
                .servers
                .get(*ix)
                .map(|s| s.status.clone())
        });
        let connecting = status == Some(Status::Connecting);
        let failed = match status {
            Some(Status::Error(text)) => Some(text),
            _ => None,
        };
        let (title, subtitle) = match (editing, opened) {
            (true, true) if failed.is_some() => (
                "Edit server",
                "The connection failed. Change the settings and connect again.",
            ),
            (true, true) => (
                "Edit server",
                "Saved, then connected with the new settings.",
            ),
            (true, false) => (
                "Disconnected",
                "Connect with these settings, or change them first.",
            ),
            (false, _) => (
                "Add server",
                "Saved to the sidebar. Connection is attempted immediately.",
            ),
        };
        let action = "Connect";
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
                        .child(
                            accent_button(cx, action, 26.)
                                .id("connect")
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
