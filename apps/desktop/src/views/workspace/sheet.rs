//! The Settings sheet over the window: opening it, closing it, and where
//! the keyboard goes when nothing over the window holds it.

use gpui_kit::{App, DismissEvent};

use super::*;

impl Workspace {
    /// Open the Settings sheet over the window, or keep the one already open.
    pub fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.is_none() {
            let sheet = cx.new(|cx| settings::Settings::new(self.state.clone(), window, cx));
            let sub = cx.subscribe_in(&sheet, window, |this, _, _: &DismissEvent, window, cx| {
                this.close_settings(window, cx)
            });
            self.settings = Some((sheet, sub));
        }
        if self.palette_open {
            self.close_palette(window, cx);
        }
        cx.notify();
    }

    /// Close the Settings sheet, if it is open, and give the keyboard back.
    pub fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.take().is_some() {
            window.focus(&self.home_focus(cx), cx);
            cx.notify();
        }
    }

    /// What holds the keyboard when nothing over the window does: the list,
    /// or the window itself while no list is drawn (no server yet, or the
    /// log zoomed over the columns), so its shortcuts still answer.
    pub(crate) fn home_focus(&self, cx: &App) -> FocusHandle {
        let s = self.state.read(cx);
        if s.servers.is_empty() || (s.drawer_open && s.drawer_zoomed) {
            self.focus.clone()
        } else {
            self.list_focus.clone()
        }
    }
}
