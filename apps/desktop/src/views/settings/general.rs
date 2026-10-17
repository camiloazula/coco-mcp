//! The General section: theme, list names and update checks.

use gpui_kit::ElementId;

use super::*;

impl Settings {
    /// One option of a choice, drawn as the app's other two-way choices are.
    /// A disabled one does nothing.
    pub(super) fn choice(
        &self,
        id: impl Into<ElementId>,
        label: &'static str,
        chosen: bool,
        enabled: bool,
        cx: &mut Context<Self>,
        on: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        text_tab(cx, label, chosen)
            .id(id.into())
            .py(px(4.))
            .when(!enabled, |el| el.opacity(0.5))
            .when(enabled, |el| {
                el.on_click(cx.listener(move |this, _, window, cx| on(this, window, cx)))
            })
            .test_support()
            .into_any_element()
    }

    /// A labelled row around any content.
    pub(super) fn row_with(
        &self,
        label: &'static str,
        content: AnyElement,
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
                    .py(px(4.))
                    .text_size(px(13.))
                    .text_color(t.muted)
                    .child(label),
            )
            .child(div().flex_1().min_w_0().child(content))
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
                    .py(px(4.))
                    .text_size(px(13.))
                    .text_color(t.muted)
                    .child(label),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(4.))
                    .child(h_flex().gap(px(16.)).children(choices))
                    .children(note.map(|n| muted(cx, 11., n).whitespace_normal())),
            )
            .into_any_element()
    }

    /// Theme, list names and update checks.
    pub(super) fn general(&self, cx: &mut Context<Self>) -> AnyElement {
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
}
