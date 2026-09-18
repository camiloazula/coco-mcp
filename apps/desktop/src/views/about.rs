//! The About window: the mark, the `coco` wordmark, the build it came from,
//! and two links. 320 × 400, not resizable, with the platform's own window
//! controls in the 36px header.

use gpui_kit::component::{TitleBar, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, AppContext as _, Bounds, Context, Entity, FocusHandle, Focusable, FontWeight,
    InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, TestSupportExt as _, Window, WindowBounds, WindowHandle,
    WindowOptions, div, px, size, svg,
};

use crate::assets;
use crate::theme::tokens;
use crate::views::{mono, muted};

/// Window size from the design.
const WINDOW: (f32, f32) = (320., 400.);
/// Height of the large mark; its viewBox is 32 × 31.
const MARK_SIZE: f32 = 72.;
/// Height of the mark standing in for the `o` in the wordmark.
const INLINE_MARK: f32 = 20.;

/// Where the two buttons go.
const SOURCE: &str = "https://github.com/camiloazula/coco-mcp";
const ISSUES: &str = "https://github.com/camiloazula/coco-mcp/issues";

/// `0.1.0 (a3f9c2e)`, or just the version when the build carries no commit.
pub fn build_line() -> String {
    match option_env!("COCO_COMMIT") {
        Some(commit) => format!("{} ({commit})", env!("CARGO_PKG_VERSION")),
        None => env!("CARGO_PKG_VERSION").to_owned(),
    }
}

/// Commit date of the build, when it has one.
pub fn build_date() -> Option<&'static str> {
    option_env!("COCO_DATE")
}

/// The About window's contents.
#[derive(Debug)]
pub struct About {
    focus: FocusHandle,
}

impl About {
    /// Build the view.
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
        }
    }

    /// The mark: outline in `fg`, eyes in `accent`, stacked as two masks.
    fn mark(&self, height: f32, bold: bool, cx: &App) -> AnyElement {
        let t = *tokens(cx);
        let (outline, eyes) = if bold {
            (assets::MARK_BOLD, assets::EYES_BOLD)
        } else {
            (assets::MARK, assets::EYES)
        };
        let layer = |path: &'static str, color| {
            svg()
                .absolute()
                .inset_0()
                .size_full()
                .path(path)
                .text_color(color)
        };
        div()
            .relative()
            .flex_none()
            .w(px(height / assets::MARK_RATIO))
            .h(px(height))
            .child(layer(outline, t.fg))
            .child(layer(eyes, t.accent))
            .into_any_element()
    }

    /// `c`, the mark, `co`: mono 28px, bottoms aligned.
    fn wordmark(&self, cx: &App) -> AnyElement {
        let letter = |text: &'static str| {
            mono(cx, 28., text)
                .flex_none()
                .font_weight(FontWeight::MEDIUM)
                .line_height(px(28.))
        };
        h_flex()
            .items_end()
            .gap(px(2.))
            .child(letter("c"))
            .child(div().mb(px(1.)).child(self.mark(INLINE_MARK, true, cx)))
            .child(letter("co"))
            .into_any_element()
    }

    /// An outlined full-width button that opens `url`.
    fn link(
        &self,
        id: &'static str,
        label: &'static str,
        url: &'static str,
        cx: &App,
    ) -> AnyElement {
        let t = *tokens(cx);
        h_flex()
            .id(id)
            .h(px(32.))
            .w_full()
            .items_center()
            .justify_center()
            .rounded(px(6.))
            .border_1()
            .border_color(t.hair)
            .text_size(px(13.))
            .text_color(t.fg)
            .cursor_pointer()
            .hover(|s| s.bg(t.hover))
            .on_click(move |_, _, _| {
                if let Err(e) = open::that_detached(url) {
                    tracing::warn!("cannot open {url}: {e}");
                }
            })
            .child(label)
            .test_support()
            .into_any_element()
    }
}

impl Focusable for About {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for About {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *tokens(cx);
        let date: Option<SharedString> = build_date().map(SharedString::from);
        v_flex()
            .id("about")
            .test_support()
            .track_focus(&self.focus)
            .size_full()
            .bg(t.bg)
            .text_color(t.fg)
            // The window controls live here; the design draws no separator,
            // so the component's bottom border is cleared.
            .child(TitleBar::new().h(px(36.)).bg(t.bg).border_b_0().flex_none())
            .child(
                v_flex()
                    .flex_1()
                    .items_center()
                    .px(px(24.))
                    .pt(px(20.))
                    .pb(px(24.))
                    .child(self.mark(MARK_SIZE, false, cx))
                    .child(div().mt(px(22.)).child(self.wordmark(cx)))
                    .child(
                        div()
                            .mt(px(8.))
                            .child(muted(cx, 13., "Debug MCP servers in depth")),
                    )
                    .child(
                        v_flex()
                            .mt(px(16.))
                            .items_center()
                            .line_height(px(20.4))
                            .child(mono(cx, 12., build_line()).text_color(t.muted))
                            .children(date.map(|d| mono(cx, 12., d).text_color(t.muted))),
                    )
                    .child(div().flex_1())
                    .child(
                        v_flex()
                            .w_full()
                            .gap(px(8.))
                            .child(self.link("source-code", "Source code", SOURCE, cx))
                            .child(self.link("report-issue", "Report an issue", ISSUES, cx)),
                    )
                    .child(div().mt(px(14.)).child(muted(cx, 11., "MIT OR Apache-2.0"))),
            )
    }
}

/// Options for the About window: fixed size, centred, platform window controls.
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

/// Open the About window, or raise the one already open.
pub fn open(
    existing: Option<&WindowHandle<gpui_kit::component::Root>>,
    cx: &mut App,
) -> Option<WindowHandle<gpui_kit::component::Root>> {
    if let Some(handle) = existing
        && handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        return None;
    }
    match cx.open_window(window_options(cx), |window, cx| {
        window.set_window_title(&format!("About {}", crate::APP_NAME));
        let about: Entity<About> = cx.new(|cx| About::new(window, cx));
        cx.new(|cx| gpui_kit::component::Root::new(about, window, cx))
    }) {
        Ok(handle) => Some(handle),
        Err(e) => {
            tracing::error!("cannot open the About window: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_build_line_carries_the_version() {
        let line = build_line();
        assert!(line.starts_with(env!("CARGO_PKG_VERSION")), "{line}");
        if let Some(commit) = option_env!("COCO_COMMIT") {
            assert!(line.ends_with(&format!("({commit})")), "{line}");
            assert_eq!(commit.len(), 7, "short hash");
        }
    }
}
