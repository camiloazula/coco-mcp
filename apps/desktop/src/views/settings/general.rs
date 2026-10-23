//! The General section: theme, list names and update checks, and the row
//! every section is drawn with.

use gpui_kit::{ElementId, SharedString};

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

    /// A row: its title with the description under it at the left, and the
    /// control at the right.
    pub(super) fn row(
        &self,
        title: impl Into<SharedString>,
        description: Vec<AnyElement>,
        control: AnyElement,
        cx: &App,
    ) -> AnyElement {
        let t = *tokens(cx);
        h_flex()
            .py(px(12.))
            .gap(px(24.))
            .border_b_1()
            .border_color(t.hair)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(3.))
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(t.fg)
                            .child(title.into()),
                    )
                    .children(description),
            )
            .child(div().flex_none().child(control))
            .into_any_element()
    }

    /// A description line under a row's title.
    pub(super) fn note(cx: &App, text: impl Into<SharedString>) -> AnyElement {
        muted(cx, 12., text).whitespace_normal().into_any_element()
    }

    /// Two or more options side by side, the row's control.
    fn choices(options: Vec<AnyElement>) -> AnyElement {
        h_flex().gap(px(16.)).children(options).into_any_element()
    }

    /// Theme, list names and update checks.
    pub(super) fn general(&self, cx: &mut Context<Self>) -> AnyElement {
        let (dark, names) = {
            let s = self.state.read(cx);
            (s.dark, s.display_names)
        };
        let (checks, env_off) = update::checks(cx);
        let theme = vec![
            self.choice("theme-dark", "Dark", dark, true, cx, |this, _, cx| {
                this.set_dark(true, cx)
            }),
            self.choice("theme-light", "Light", !dark, true, cx, |this, _, cx| {
                this.set_dark(false, cx)
            }),
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
        let updates = self.switch(
            "update-checks",
            "Update checks",
            checks,
            !env_off,
            cx,
            |next, _, cx| {
                if next != update::checks(cx).0 {
                    update::toggle_checks(cx);
                }
            },
        );
        let mut updates_note = vec![Self::note(
            cx,
            "Coco looks for a new release when it starts.",
        )];
        if env_off {
            updates_note.push(Self::note(
                cx,
                "Off in the environment (COCO_NO_UPDATE_CHECK or DO_NOT_TRACK).",
            ));
        }
        v_flex()
            .child(self.row(
                "Theme",
                vec![Self::note(cx, "How every Coco window is drawn.")],
                Self::choices(theme),
                cx,
            ))
            .child(self.row(
                "List names",
                vec![Self::note(
                    cx,
                    "What the Tools, Resources and Prompts lists show.",
                )],
                Self::choices(list_names),
                cx,
            ))
            .child(self.row("Update checks", updates_note, updates, cx))
            .into_any_element()
    }
}
