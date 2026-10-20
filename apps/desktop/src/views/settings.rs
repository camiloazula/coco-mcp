//! The Settings window: Coco's own choices in one place, with a section list
//! on the left. The menu items, palette entries and buttons that change the
//! same choices stay, and a change made anywhere shows in every window.
//! 560 × 720, not resizable, with the platform's window controls in the 36px
//! header.

use std::collections::HashMap;

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{Root, TitleBar, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Bounds, Context, Entity, FocusHandle, Focusable, Global,
    InteractiveElement, IntoElement, ParentElement, Render, StatefulInteractiveElement, Styled,
    Subscription, TestSupportExt as _, Window, WindowBounds, WindowHandle, WindowOptions, div, px,
    size,
};

use crate::actions::{ToggleDisplayNames, ToggleTheme};
use crate::state::AppState;
use crate::theme::{self, tokens};
use crate::update;
use crate::views::{muted, text_tab};

mod gateway;
mod general;
mod switch;

/// Window size.
const WINDOW: (f32, f32) = (560., 720.);
/// Width of the section list.
const NAV: f32 = 140.;
/// Width of a row's label.
const LABEL: f32 = 130.;

/// A section of the window.
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

    fn id(self) -> &'static str {
        match self {
            Self::General => "section-general",
            Self::Gateway => "section-gateway",
        }
    }
}

/// The model the window changes, set once the app starts: the Settings action
/// is global and reaches it from whichever window is in front.
#[derive(Debug, Clone)]
pub struct Model(pub Entity<AppState>);

impl Global for Model {}

/// The window while one is open, so a second request raises it.
#[derive(Debug, Default)]
struct Opened(Option<WindowHandle<Root>>);

impl Global for Opened {}

/// The Settings window's contents.
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

    fn set_dark(&mut self, dark: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.state.update(cx, |s, cx| s.set_dark(dark, cx));
        // The main window applies it too; this window must not wait for that.
        theme::set_dark(dark, Some(window), cx);
    }
}

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
            div()
                .id(section.id())
                .h(px(28.))
                .px(px(12.))
                .flex()
                .items_center()
                .text_size(px(13.))
                .text_color(if chosen { t.fg } else { t.muted })
                .when(chosen, |el| el.bg(t.sel))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.section = section;
                    cx.notify();
                }))
                .child(section.label())
                .test_support()
        });
        let body = match self.section {
            Section::General => self.general(cx),
            Section::Gateway => self.gateway(window, cx),
        };
        v_flex()
            .id("settings")
            .test_support()
            .track_focus(&self.focus)
            .size_full()
            .bg(t.bg)
            .text_color(t.fg)
            .child(TitleBar::new().h(px(36.)).bg(t.bg).flex_none())
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        v_flex()
                            .w(px(NAV))
                            .flex_none()
                            .h_full()
                            .py(px(8.))
                            .bg(t.sunk)
                            .border_r_1()
                            .border_color(t.hair)
                            .children(nav),
                    )
                    .child(
                        v_flex()
                            .id("settings-body")
                            .flex_1()
                            .min_w_0()
                            .overflow_y_scroll()
                            .px(px(24.))
                            .py(px(14.))
                            .child(
                                div()
                                    .pb(px(6.))
                                    .text_size(px(15.))
                                    .child(self.section.label()),
                            )
                            .child(body),
                    ),
            )
    }
}

/// Options for the Settings window: fixed size, centred, platform controls.
pub fn window_options(cx: &App) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(WINDOW.0), px(WINDOW.1)),
            cx,
        ))),
        is_resizable: false,
        is_minimizable: false,
        ..TitleBar::window_options()
    }
}

/// Open the Settings window over `state`, or raise the one already open.
pub fn open(
    state: &Entity<AppState>,
    existing: Option<&WindowHandle<Root>>,
    cx: &mut App,
) -> Option<WindowHandle<Root>> {
    if let Some(handle) = existing
        && handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        return None;
    }
    let state = state.clone();
    match cx.open_window(window_options(cx), move |window, cx| {
        window.set_window_title("Settings");
        let settings: Entity<Settings> = cx.new(|cx| Settings::new(state, window, cx));
        cx.new(|cx| Root::new(settings, window, cx))
    }) {
        Ok(handle) => Some(handle),
        Err(e) => {
            tracing::error!("cannot open the Settings window: {e}");
            None
        }
    }
}

/// Open the window the application menu asks for, over the app's model.
pub fn show(cx: &mut App) {
    let Some(model) = cx.try_global::<Model>().map(|m| m.0.clone()) else {
        return;
    };
    let existing = cx.try_global::<Opened>().and_then(|o| o.0);
    if let Some(handle) = open(&model, existing.as_ref(), cx) {
        cx.set_global(Opened(Some(handle)));
    }
}

/// Register what the window's own choices do from the View menu while a
/// Settings window is in front: the menu only enables an action the active
/// window or the application answers, and these are the workspace's.
pub fn install(cx: &mut App) {
    cx.on_action(|_: &ToggleTheme, cx| {
        let Some(model) = cx.try_global::<Model>().map(|m| m.0.clone()) else {
            return;
        };
        let dark = !model.read(cx).dark;
        model.update(cx, |s, cx| s.set_dark(dark, cx));
        theme::set_dark(dark, None, cx);
    });
    cx.on_action(|_: &ToggleDisplayNames, cx| {
        let Some(model) = cx.try_global::<Model>().map(|m| m.0.clone()) else {
            return;
        };
        let on = !model.read(cx).display_names;
        model.update(cx, |s, cx| s.set_display_names(on, cx));
    });
}
