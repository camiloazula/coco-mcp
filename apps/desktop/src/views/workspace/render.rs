//! Drawing the workspace: title bar, columns, log drawer and overlays, and
//! the actions the window answers.

use super::*;

impl Workspace {
    pub(super) fn title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *tokens(cx);
        TitleBar::new().h(px(36.)).bg(t.sunk).child(
            div()
                .flex_1()
                .text_center()
                .text_size(px(12.))
                .text_color(t.muted)
                .child(crate::APP_NAME),
        )
    }

    pub(super) fn columns(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = *tokens(cx);
        let (has_servers, is_form) = {
            let s = self.state.read(cx);
            (!s.servers.is_empty(), s.screen == Screen::AddServer)
        };
        let detail: AnyElement = match (&self.add_form, is_form) {
            (Some(form), true) => form.clone().into_any_element(),
            _ => detail::render(self, window, cx),
        };
        let sidebar = sidebar::render(self, &self.server_focus.clone(), window, cx);
        let banner = (!is_form).then(|| banner::render(self, cx)).flatten();
        let detail_pane = v_flex()
            .size_full()
            .min_w_0()
            .bg(t.bg)
            .children(banner)
            .child(detail);
        if !has_servers {
            // No list column yet: a plain two-column layout, so the resizable
            // group below is created with the design sizes once servers exist.
            return h_flex()
                .size_full()
                .items_start()
                .child(div().w(px(200.)).h_full().flex_none().child(sidebar))
                .child(div().w(px(1.)).h_full().flex_none().bg(t.hair))
                .child(div().flex_1().h_full().min_w_0().child(detail_pane))
                .into_any_element();
        }
        let list = item_list::render(
            self,
            &self.filter.clone(),
            &self.list_focus.clone(),
            window,
            cx,
        );
        h_resizable("columns")
            .with_state(&self.columns)
            .child(
                resizable_panel()
                    .size(px(200.))
                    .size_range(px(160.)..px(400.))
                    .child(sidebar),
            )
            .child(
                resizable_panel()
                    .size(px(280.))
                    .size_range(px(200.)..px(600.))
                    .child(list),
            )
            .child(resizable_panel().child(detail_pane))
            .into_any_element()
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *tokens(cx);
        self.sync_filter_placeholder(window, cx);
        self.sync_level_select(window, cx);
        let wanted = self.wanted_form(self.state.read(cx));
        // A form is built for one target: `+` while editing stays on the
        // screen, but the old form would still save over the edited server.
        if wanted != self.add_form_target {
            self.add_form = None;
            self.add_form_target = None;
        }
        if let Some((editing, opened)) = wanted.clone()
            && self.add_form.is_none()
        {
            let form = cx.new(|cx| AddServerForm::new(self.state.clone(), editing, window, cx));
            // A form opened on purpose takes the keyboard; one shown for an
            // off server leaves it with the list, so ↑ ↓ still move.
            if opened {
                form.update(cx, |f, cx| f.focus(window, cx));
            }
            self.add_form = Some(form);
            self.add_form_target = wanted;
        }
        let (drawer_open, drawer_zoomed) = {
            let s = self.state.read(cx);
            (s.drawer_open, s.drawer_zoomed)
        };
        let dialogs = Root::render_dialog_layer(window, cx);
        let sheets = Root::render_sheet_layer(window, cx);
        let notifications = Root::render_notification_layer(window, cx);

        // Every blob drawn by the columns is marked, and the rest dropped.
        self.decoded.begin_frame();
        let main = self.columns(window, cx);
        self.decoded.end_frame();
        let overlay = request_dialog::render(self, window, cx);
        let confirm = confirm_dialog::render(self, window, cx);
        // A dialog takes the keyboard while it is open, and gives it back to
        // the list when it closes.
        let dialog_open = overlay.is_some() || confirm.is_some();
        let dialog_focused = self.dialog_focus.contains_focused(window, cx);
        if dialog_open != dialog_focused {
            let target = if dialog_open {
                self.dialog_focus.clone()
            } else {
                self.list_focus.clone()
            };
            window.defer(cx, move |window, cx| window.focus(&target, cx));
        }
        let palette = palette::render(self, window, cx);
        let copy_menu = copy_menu::render(cx);
        let header = log_drawer::header(self, cx);
        let middle: AnyElement = if drawer_open && drawer_zoomed {
            // The drawer alone between the title bar and the status bar; the
            // columns keep their state and come back at the same sizes.
            let body = log_drawer::body(self, window, cx);
            v_flex()
                .id("log-zoomed")
                .size_full()
                .child(header)
                .child(body)
                .test_support()
                .into_any_element()
        } else if drawer_open {
            let body = log_drawer::body(self, window, cx);
            v_resizable("rows")
                .with_state(&self.rows)
                .child(resizable_panel().child(main))
                .child(
                    resizable_panel()
                        .size(px(260.))
                        .size_range(px(80.)..px(600.))
                        .child(v_flex().size_full().child(header).child(body)),
                )
                .into_any_element()
        } else {
            v_flex()
                .size_full()
                .child(div().flex_1().min_h_0().child(main))
                .child(header)
                .into_any_element()
        };

        v_flex()
            .id("workspace")
            .test_support()
            .key_context(WORKSPACE)
            .track_focus(&self.focus)
            .on_action(
                cx.listener(|this, _: &AddServer, window, cx| this.open_add_server(window, cx)),
            )
            .on_action(cx.listener(Self::on_call))
            .on_action(cx.listener(|this, _: &CancelCall, _, cx| {
                this.state.update(cx, |s, cx| s.cancel_response(cx));
            }))
            .on_action(cx.listener(Self::on_cancel))
            .on_action(cx.listener(|this, _: &ToggleLog, _, cx| {
                this.state.update(cx, |s, cx| s.toggle_drawer(cx));
            }))
            .on_action(cx.listener(|this, _: &ZoomLog, _, cx| {
                this.state.update(cx, |s, cx| s.toggle_drawer_zoom(cx));
            }))
            .on_action(cx.listener(|this, _: &ToggleResponse, _, cx| {
                this.state.update(cx, |s, cx| s.toggle_response(cx));
            }))
            .on_action(cx.listener(|this, _: &ClearLog, _, cx| {
                this.state.update(cx, |s, cx| s.clear_log(cx));
            }))
            .on_action(cx.listener(|this, _: &ToggleTheme, _, cx| {
                let dark = !this.state.read(cx).dark;
                this.state.update(cx, |s, cx| s.set_dark(dark, cx));
            }))
            .on_action(cx.listener(|this, _: &ShowTools, _, cx| {
                this.state.update(cx, |s, cx| s.set_mode(Mode::Tools, cx));
            }))
            .on_action(cx.listener(|this, _: &ShowResources, _, cx| {
                this.state
                    .update(cx, |s, cx| s.set_mode(Mode::Resources, cx));
            }))
            .on_action(cx.listener(|this, _: &ShowPrompts, _, cx| {
                this.state.update(cx, |s, cx| s.set_mode(Mode::Prompts, cx));
            }))
            .on_action(cx.listener(|this, _: &ShowHistory, _, cx| {
                this.state.update(cx, |s, cx| s.set_mode(Mode::History, cx));
            }))
            .on_action(cx.listener(|this, _: &ShowServer, _, cx| {
                this.state.update(cx, |s, cx| s.set_mode(Mode::Server, cx));
            }))
            .on_action(cx.listener(|this, _: &ClearHistory, _, cx| {
                this.state.update(cx, |s, cx| s.request_clear_history(cx));
            }))
            .on_action(cx.listener(|this, _: &ForgetCredentials, _, cx| {
                this.state
                    .update(cx, |s, cx| s.request_forget_credentials(cx));
            }))
            .on_action(
                cx.listener(|this, _: &CompareSnapshotFile, _, cx| this.compare_snapshot_file(cx)),
            )
            .on_action(cx.listener(|this, action: &SelectServer, _, cx| {
                let index = action.index;
                this.state.update(cx, |s, cx| s.select_server(index, cx));
            }))
            .on_action(cx.listener(|this, _: &EditServer, window, cx| {
                // Where the pane already shows the server's settings, go to
                // them rather than open a second copy over them.
                if this.state.read(cx).settings_pane().is_some()
                    && let Some(form) = this.server_form()
                {
                    form.update(cx, |f, cx| f.focus(window, cx));
                    return;
                }
                this.state.update(cx, |s, cx| s.show_edit_selected(cx));
            }))
            .on_action(cx.listener(|this, _: &DeleteServer, _, cx| {
                this.state.update(cx, |s, cx| s.request_delete_selected(cx));
            }))
            .on_action(cx.listener(|this, _: &Disconnect, _, cx| {
                this.state.update(cx, |s, cx| {
                    if let Some(ix) = s.selected_server {
                        s.disconnect(ix, cx);
                    }
                });
            }))
            .on_action(cx.listener(|this, _: &Reconnect, _, cx| {
                this.state.update(cx, |s, cx| {
                    if let Some(ix) = s.selected_server {
                        s.connect(ix, cx);
                    }
                });
            }))
            .on_action(cx.listener(|this, _: &CopyResponse, _, cx| this.copy_response(cx)))
            .on_action(cx.listener(|this, _: &CopyRequest, _, cx| this.copy_request(cx)))
            .on_action(cx.listener(|this, _: &CopyRequestCurl, _, cx| this.copy_request_curl(cx)))
            .on_action(cx.listener(|this, _: &CopyServerConfig, _, cx| this.copy_server_config(cx)))
            .on_action(cx.listener(|this, _: &CopyBearerToken, _, cx| this.copy_bearer_token(cx)))
            .on_action(
                cx.listener(|this, _: &ExportSnapshot, _, cx| {
                    this.export_file(Export::Snapshot, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &ExportLog, _, cx| this.export_file(Export::Log, cx)))
            .on_action(
                cx.listener(|this, _: &ExportHistory, _, cx| this.export_file(Export::History, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ExportConfig, _, cx| this.export_file(Export::Config, cx)),
            )
            .on_action(cx.listener(|this, _: &ImportConfig, _, cx| this.import_config_file(cx)))
            .on_action(cx.listener(|this, _: &CommandPalette, window, cx| {
                if this.palette_open {
                    this.close_palette(window, cx);
                } else {
                    this.open_palette(window, cx);
                }
            }))
            .on_action(|_: &Quit, _, cx| cx.quit())
            .relative()
            .size_full()
            .bg(t.bg)
            .text_color(t.fg)
            .text_size(px(13.))
            .line_height(px(crate::views::BODY_LINE_HEIGHT))
            .child(self.title_bar(cx))
            .child(div().flex_1().min_h_0().child(middle))
            .child(status_bar::render(self, cx))
            .children(overlay)
            .children(confirm)
            .children(palette)
            .children(copy_menu)
            .children(dialogs)
            .children(sheets)
            .children(notifications)
    }
}
