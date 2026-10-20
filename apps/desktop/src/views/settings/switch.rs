//! An on/off choice as a switch: the pill, and "On" or "Off" beside it.

use std::rc::Rc;

use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Disableable as _, Sizable as _, Size};
use gpui_kit::{ElementId, SharedString};

use super::*;

impl Settings {
    /// A switch named `name` for a screen reader, showing `on`, with its state in words at its right. `change`
    /// is given the value the person asked for; the model it writes is read
    /// back on the next frame, so nothing is kept here. A disabled switch
    /// does nothing. The word is the app's 13px text and takes the click too,
    /// as the switch's own label would, which draws at 14px.
    pub(super) fn switch(
        &self,
        id: impl Into<ElementId>,
        name: impl Into<SharedString>,
        on: bool,
        enabled: bool,
        cx: &App,
        change: impl Fn(bool, &mut Window, &mut App) + 'static,
    ) -> AnyElement {
        let t = *tokens(cx);
        let id: ElementId = id.into();
        let change = Rc::new(change);
        let toggle = change.clone();
        h_flex()
            .gap(px(8.))
            .items_center()
            .child(
                Switch::new(id.clone())
                    .accessibility_label(name)
                    .checked(on)
                    .with_size(Size::Small)
                    .disabled(!enabled)
                    .on_change(move |next, window, cx| toggle(*next, window, cx)),
            )
            .child(
                div()
                    .id((id, "word"))
                    .text_size(px(13.))
                    .text_color(if on { t.fg } else { t.muted })
                    .when(enabled, |el| {
                        el.cursor_pointer()
                            .on_click(move |_, window, cx| change(!on, window, cx))
                    })
                    .when(!enabled, |el| el.opacity(0.5))
                    .child(if on { "On" } else { "Off" })
                    .test_support(),
            )
            .into_any_element()
    }
}
