//! The Settings window: Coco's own choices in one place, with a section list
//! on the left. The menu items, palette entries and buttons that change the
//! same choices stay, and a change made anywhere shows in every window.
//! 560 × 380, not resizable, with the platform's window controls in the 36px
//! header.

use gpui_kit::component::{Root, TitleBar, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Bounds, Context, Entity, FocusHandle, Focusable, Global,
    InteractiveElement, IntoElement, ParentElement, Render, StatefulInteractiveElement, Styled,
    Subscription, TestSupportExt as _, Window, WindowBounds, WindowHandle, WindowOptions, div, px,
    size,
};

use crate::state::AppState;
use crate::theme::{self, tokens};
use crate::update;
use crate::views::muted;

/// Window size.
const WINDOW: (f32, f32) = (560., 380.);
/// Width of the section list.
const NAV: f32 = 140.;
/// Width of a row's label.
const LABEL: f32 = 130.;

/// A section of the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    /// Theme, list names, update checks.
    General,
}

impl Section {
    /// Every section, in list order.
    pub const ALL: [Section; 1] = [Section::General];

    fn label(self) -> &'static str {
        match self {
            Self::General => "General",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::General => "section-general",
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
    _observe: Subscription,
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
    pub fn new(state: Entity<AppState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&state, |_, _, cx| cx.notify());
        Self {
            state,
            section: Section::General,
            focus: cx.focus_handle(),
            _observe: observe,
        }
    }

    /// One option of a choice: outlined, in the accent colour while chosen.
    /// A disabled one does nothing.
    fn choice(
        &self,
        id: &'static str,
        label: &'static str,
        chosen: bool,
        enabled: bool,
        cx: &mut Context<Self>,
        on: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        let t = *tokens(cx);
        div()
            .id(id)
            .h(px(26.))
            .px(px(12.))
            .flex()
            .items_center()
            .rounded(px(4.))
            .border_1()
            .border_color(if chosen { t.accent } else { t.hair })
            .bg(if chosen { t.sel } else { t.bg })
            .text_size(px(12.))
            .text_color(if chosen { t.accent } else { t.fg })
            .when(!enabled, |el| el.opacity(0.5))
            .when(enabled, |el| {
                el.cursor_pointer()
                    .hover(|s| s.bg(t.hover))
                    .on_click(cx.listener(move |this, _, window, cx| on(this, window, cx)))
            })
            .child(label)
            .test_support()
            .into_any_element()
    }

    /// A labelled row: the label, the choices, and a note under them.
    fn row(
        &self,
        label: &'static str,
        choices: Vec<AnyElement>,
        note: Option<&'static str>,
        cx: &App,
    ) -> AnyElement {
        let t = *tokens(cx);
        h_flex()
            .py(px(10.))
            .border_b_1()
            .border_color(t.hair)
            .items_start()
            .child(
                div()
                    .w(px(LABEL))
                    .flex_none()
                    .pt(px(4.))
                    .text_size(px(13.))
                    .text_color(t.muted)
                    .child(label),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(6.))
                    .child(h_flex().gap(px(6.)).children(choices))
                    .children(note.map(|n| muted(cx, 11., n).whitespace_normal())),
            )
            .into_any_element()
    }

    /// Theme, list names and update checks.
    fn general(&self, cx: &mut Context<Self>) -> AnyElement {
        let (dark, names) = {
            let s = self.state.read(cx);
            (s.dark, s.display_names)
        };
        let (checks, env_off) = update::checks(cx);
        let theme = vec![
            self.choice("theme-dark", "Dark", dark, true, cx, |this, window, cx| {
                this.set_dark(true, window, cx)
            }),
            self.choice(
                "theme-light",
                "Light",
                !dark,
                true,
                cx,
                |this, window, cx| this.set_dark(false, window, cx),
            ),
        ];
        let list_names = vec![
            self.choice(
                "names-identifiers",
                "Identifiers",
                !names,
                true,
                cx,
                |this, _, cx| {
                    this.state
                        .update(cx, |s, cx| s.set_display_names(false, cx))
                },
            ),
            self.choice(
                "names-display",
                "Display names",
                names,
                true,
                cx,
                |this, _, cx| this.state.update(cx, |s, cx| s.set_display_names(true, cx)),
            ),
        ];
        let updates = vec![
            self.choice("checks-on", "On", checks, !env_off, cx, |_, _, cx| {
                if !update::checks(cx).0 {
                    update::toggle_checks(cx);
                }
            }),
            self.choice("checks-off", "Off", !checks, !env_off, cx, |_, _, cx| {
                if update::checks(cx).0 {
                    update::toggle_checks(cx);
                }
            }),
        ];
        v_flex()
            .child(self.row("Theme", theme, None, cx))
            .child(self.row(
                "List names",
                list_names,
                Some("What the Tools, Resources and Prompts lists show."),
                cx,
            ))
            .child(self.row(
                "Update checks",
                updates,
                env_off.then_some("Off in the environment (COCO_NO_UPDATE_CHECK or DO_NOT_TRACK)."),
                cx,
            ))
            .into_any_element()
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
                            .flex_1()
                            .min_w_0()
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
