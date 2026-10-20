//! An on/off choice as a switch: a pill with a round thumb, and "on" or
//! "off" beside it in the track's colour, as the reference draws it.

use std::rc::Rc;

use gpui_kit::{ElementId, Role, SharedString, Toggled};

use super::*;

impl Settings {
    /// A switch showing `on`, named `name` for a screen reader. `change` is
    /// given the value the person asked for; the model it writes is read
    /// back on the next frame, so nothing is kept here. A disabled switch
    /// does nothing. The word beside the track takes the click too.
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
        let (track_change, word_change) = (change.clone(), change);
        let thumb = div()
            .absolute()
            .top(px(2.))
            .when(on, |el| el.right(px(2.)))
            .when(!on, |el| el.left(px(2.)))
            .size(px(16.))
            .rounded_full()
            .bg(t.sunk);
        // Dimmed by its track colour, not by the element's opacity: opacity
        // multiplies down the subtree and would show the track through the thumb.
        let track_bg = if on { t.accent } else { t.hair };
        let track = div()
            .id(id.clone())
            .relative()
            .flex_none()
            .w(px(36.))
            .h(px(20.))
            .rounded_full()
            .bg(if enabled {
                track_bg
            } else {
                track_bg.opacity(0.5)
            })
            .role(Role::Switch)
            .aria_toggled(if on { Toggled::True } else { Toggled::False })
            .aria_label(name)
            .when(enabled, |el| {
                el.cursor_pointer()
                    .on_click(move |_, window, cx| track_change(!on, window, cx))
            })
            .child(thumb)
            .test_support();
        let word = div()
            .id((id, "word"))
            .text_size(px(13.))
            .text_color(if on { t.accent } else { t.muted })
            .when(!enabled, |el| el.opacity(0.5))
            .when(enabled, |el| {
                el.cursor_pointer()
                    .on_click(move |_, window, cx| word_change(!on, window, cx))
            })
            .child(if on { "on" } else { "off" })
            .test_support();
        h_flex()
            .gap(px(8.))
            .items_center()
            .child(track)
            .child(word)
            .into_any_element()
    }
}
