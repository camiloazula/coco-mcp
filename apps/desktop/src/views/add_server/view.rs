//! Drawing the add-server form.

use super::*;

/// What choosing `mode` means, beside its tabs.
fn protocol_hint(mode: ProtocolMode) -> &'static str {
    match mode {
        ProtocolMode::Legacy => "initialize handshake · 2025-11-25 and older",
        ProtocolMode::Auto => "2026-07-28 when the server has it, else the handshake",
        ProtocolMode::Modern => "2026-07-28 only",
    }
}

impl Render for AddServerForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *tokens(cx);
        let state = self.state.clone();
        let editing = self.is_editing();
        let (title, subtitle, action) = if editing {
            (
                "Edit server",
                "Changes are saved and the server reconnects.",
                "Save",
            )
        } else {
            (
                "Add server",
                "Saved to the sidebar. Connection is attempted immediately.",
                "Connect",
            )
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
            .child(
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
            );
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
        let protocol = h_flex()
            .gap(px(16.))
            .child(
                text_tab(cx, "Legacy", self.protocol == ProtocolMode::Legacy)
                    .id("protocol-legacy")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.protocol = ProtocolMode::Legacy;
                        cx.notify();
                    }))
                    .test_support(),
            )
            .child(
                text_tab(cx, "Auto", self.protocol == ProtocolMode::Auto)
                    .id("protocol-auto")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.protocol = ProtocolMode::Auto;
                        cx.notify();
                    }))
                    .test_support(),
            )
            .child(
                text_tab(cx, "Modern", self.protocol == ProtocolMode::Modern)
                    .id("protocol-modern")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.protocol = ProtocolMode::Modern;
                        cx.notify();
                    }))
                    .test_support(),
            )
            .child(muted(cx, 12., protocol_hint(self.protocol)));
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
                        })),
                ),
        );
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
