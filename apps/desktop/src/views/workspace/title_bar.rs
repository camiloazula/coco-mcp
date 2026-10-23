//! The 36px title bar: the name centred, and the way into Settings at the
//! right, where the platform's own controls are not.

use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, Sizable as _};
use gpui_kit::{MouseButton, StatefulInteractiveElement};

use super::*;

/// The width the platform keeps at the left of the bar, for its traffic
/// lights (gpui-component's own padding), which the right slot repeats so
/// the name stays in the middle of the window. Elsewhere the bar has a
/// 12px margin and the platform's controls sit past its right end.
const SLOT: f32 = if cfg!(target_os = "macos") { 80. } else { 28. };
const LEFT_MARGIN: f32 = if cfg!(target_os = "macos") { 80. } else { 12. };

impl Workspace {
    pub(super) fn title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *tokens(cx);
        // Muted until hovered like the log drawer's buttons. A press on it
        // is not the start of a window drag.
        let settings = div()
            .id("open-settings")
            .p(px(2.))
            .text_color(t.muted)
            .hover(move |s| s.text_color(t.fg))
            .cursor_pointer()
            .tooltip(move |window, cx| Tooltip::new("Settings  ⌘,").build(window, cx))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, _, window, cx| this.open_settings(window, cx)))
            .child(Icon::new(gpui_kit::assets::IconName::Settings).with_size(px(16.)))
            .test_support();
        TitleBar::new()
            .h(px(36.))
            .bg(t.sunk)
            .child(div().w(px(SLOT - LEFT_MARGIN)).flex_none())
            .child(
                div()
                    .flex_1()
                    .text_center()
                    .text_size(px(12.))
                    .text_color(t.muted)
                    .child(crate::APP_NAME),
            )
            .child(
                h_flex()
                    .w(px(SLOT))
                    .flex_none()
                    .justify_end()
                    .pr(px(12.))
                    .child(settings),
            )
    }
}
