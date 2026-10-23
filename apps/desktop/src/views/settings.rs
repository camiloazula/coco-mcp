//! Settings: Coco's own choices in one place, as a sheet over the main
//! window below its title bar, on a dimmed backdrop. A sidebar lists the
//! sections; each row has its title and a description at the left and its
//! control at the right. The sheet closes with its × or Esc, and while it is
//! open the window's shortcuts are inert, as under a dialog. The menu items,
//! palette entries and buttons that change the same choices stay, and a
//! change made anywhere shows at once.

use std::collections::HashMap;

use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, DismissEvent, Entity, EventEmitter, FocusHandle,
    Focusable, FontWeight, InteractiveElement, IntoElement, ParentElement, Render,
    StatefulInteractiveElement, Styled, Subscription, TestSupportExt as _, Window, div, hsla, px,
};

use crate::actions::{Cancel, DIALOG, DialogAccept};
use crate::state::AppState;
use crate::theme::tokens;
use crate::update;
use crate::views::{muted, text_tab};

mod gateway;
mod general;
mod switch;

/// The largest the sheet grows to; a smaller window gives it less.
const MAX: (f32, f32) = (880., 640.);
/// The backdrop the sheet leaves around it.
const MARGIN: f32 = 32.;
/// Width of the sidebar.
const NAV: f32 = 200.;
/// Height of the main window's title bar, which the backdrop leaves clear so
/// the window can still be moved.
const TITLE_BAR: f32 = 36.;

/// A section of the sheet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    /// Theme, list names, update checks.
    General,
    /// The gateway agents reach servers through.
    Gateway,
}

impl Section {
    /// Every section, in list order.
    pub const ALL: [Section; 2] = [Section::General, Section::Gateway];

    fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Gateway => "Gateway",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Self::General => IconName::Settings,
            Self::Gateway => IconName::Network,
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::General => "section-general",
            Self::Gateway => "section-gateway",
        }
    }
}

/// The sheet's contents. It emits [`DismissEvent`] when it asks to close;
/// the window that holds it drops it.
pub struct Settings {
    state: Entity<AppState>,
    section: Section,
    focus: FocusHandle,
    /// The port being typed.
    port: Entity<InputState>,
    port_error: Option<String>,
    /// The path being typed for each server, by id, made as they are shown.
    namespaces: HashMap<String, Entity<InputState>>,
    /// A path that was refused, and for which server.
    namespace_error: Option<(String, String)>,
    subs: Vec<Subscription>,
}

impl std::fmt::Debug for Settings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Settings")
            .field("section", &self.section)
            .finish_non_exhaustive()
    }
}

impl Settings {
    /// Build the view over the app's model.
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut subs = vec![cx.observe(&state, |_, _, cx| cx.notify())];
        let shown = state.read(cx).gateway.settings.port.to_string();
        let port = cx.new(|cx| InputState::new(window, cx).default_value(shown));
        subs.push(
            cx.subscribe_in(&port, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.apply_port(cx);
                }
            }),
        );
        Self {
            state,
            section: Section::General,
            focus: cx.focus_handle(),
            port,
            port_error: None,
            namespaces: HashMap::new(),
            namespace_error: None,
            subs,
        }
    }

    /// The window holding the sheet applies the theme once the model has it.
    fn set_dark(&mut self, dark: bool, cx: &mut Context<Self>) {
        self.state.update(cx, |s, cx| s.set_dark(dark, cx));
    }
}

impl EventEmitter<DismissEvent> for Settings {}

impl Focusable for Settings {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Settings {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *tokens(cx);
        let nav = Section::ALL.map(|section| {
            let chosen = section == self.section;
            h_flex()
                .id(section.id())
                .h(px(28.))
                .px(px(10.))
                .gap(px(10.))
                .rounded(px(3.))
                .text_size(px(13.))
                .text_color(if chosen { t.fg } else { t.muted })
                .when(chosen, |el| el.bg(t.sel))
                .when(!chosen, |el| {
                    el.hover(move |s| s.bg(t.hover).text_color(t.fg))
                })
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.section = section;
                    cx.notify();
                }))
                .child(Icon::new(section.icon()).with_size(px(15.)))
                .child(section.label())
                .test_support()
        });
        let body = match self.section {
            Section::General => self.general(cx),
            Section::Gateway => self.gateway(window, cx),
        };
        let close = div()
            .id("close-settings")
            .p(px(4.))
            .rounded(px(3.))
            .text_color(t.muted)
            .hover(move |s| s.text_color(t.fg).bg(t.hover))
            .cursor_pointer()
            .tooltip(|window, cx| Tooltip::new("Close  Esc").build(window, cx))
            .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent)))
            .child(Icon::new(IconName::Close).with_size(px(16.)))
            .test_support();
        div()
            .id("settings")
            .test_support()
            .key_context(DIALOG)
            .track_focus(&self.focus)
            .on_action(cx.listener(|_, _: &Cancel, _, cx| cx.emit(DismissEvent)))
            // The sheet has nothing to accept: answering ⌘⏎ and Enter here
            // keeps them from going on to the window's Call behind it.
            .on_action(|_: &DialogAccept, _, _| {})
            .flex()
            .size_full()
            .max_w(px(MAX.0))
            .max_h(px(MAX.1))
            .bg(t.bg)
            .text_color(t.fg)
            .border_1()
            .border_color(t.hair)
            .rounded(px(3.))
            .shadow_lg()
            .child(
                v_flex()
                    .w(px(NAV))
                    .flex_none()
                    .h_full()
                    .p(px(8.))
                    .gap(px(2.))
                    .bg(t.sunk)
                    .rounded_tl(px(3.))
                    .rounded_bl(px(3.))
                    .border_r_1()
                    .border_color(t.hair)
                    .child(
                        div()
                            .px(px(10.))
                            .pt(px(8.))
                            .pb(px(6.))
                            .text_size(px(11.))
                            .text_color(t.muted)
                            .child("Settings"),
                    )
                    .children(nav),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(
                        h_flex()
                            .flex_none()
                            .justify_between()
                            .pl(px(28.))
                            .pr(px(14.))
                            .pt(px(14.))
                            .child(
                                div()
                                    .pt(px(8.))
                                    .text_size(px(15.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(self.section.label()),
                            )
                            .child(close),
                    )
                    .child(
                        div()
                            .id("settings-body")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .px(px(28.))
                            .pt(px(8.))
                            .pb(px(20.))
                            .child(body),
                    ),
            )
    }
}

/// The sheet over the window, below its title bar, on the dimmed backdrop.
pub fn overlay(settings: &Entity<Settings>) -> AnyElement {
    div()
        .id("settings-overlay")
        .occlude()
        .absolute()
        .top(px(TITLE_BAR))
        .left_0()
        .right_0()
        .bottom_0()
        .flex()
        .items_center()
        .justify_center()
        .p(px(MARGIN))
        .bg(hsla(0., 0., 0., 0.5))
        .child(settings.clone())
        .test_support()
        .into_any_element()
}
