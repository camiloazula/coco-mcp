//! What the detail pane shows with nothing to detail: no server, a server
//! that is not connected, or nothing selected.

use super::*;

pub(super) fn centered_note(cx: &Context<Workspace>, text: impl Into<SharedString>) -> AnyElement {
    v_flex()
        .flex_1()
        .items_center()
        .justify_center()
        .text_size(px(13.))
        .text_color(tokens(cx).muted)
        .child(text.into())
        .into_any_element()
}

/// A saved server that is not connected, when its settings form is not
/// there: a button that does what `⌘R`, Enter on the row and the palette
/// also do. Same layout as the empty state so the two read as one family.
pub(super) fn connect_state(ws: &Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let t = *tokens(cx);
    let entity = ws.state.clone();
    v_flex()
        .flex_1()
        .items_center()
        .justify_center()
        .gap(px(8.))
        .text_color(t.muted)
        .child(
            div()
                .text_size(px(15.))
                .text_color(t.fg)
                .child("Disconnected"),
        )
        .child(
            h_flex()
                .gap(px(16.))
                .mt(px(12.))
                .items_center()
                .child(
                    accent_button(cx, "Connect", 26.)
                        .id("connect-server")
                        .on_click(move |_, _, cx| {
                            entity.update(cx, |s, cx| {
                                if let Some(ix) = s.selected_server {
                                    s.connect(ix, cx);
                                }
                            });
                        })
                        .test_support(),
                )
                .child(kbd(cx, "⌘R")),
        )
        .into_any_element()
}

/// "No servers yet" screen from the design.
pub fn empty_state(ws: &Workspace, cx: &mut Context<Workspace>) -> Div {
    let t = *tokens(cx);
    let entity = ws.state.clone();
    v_flex()
        .flex_1()
        .items_center()
        .justify_center()
        .gap(px(8.))
        .text_color(t.muted)
        .child(div().text_size(px(15.)).text_color(t.fg).child("No servers yet"))
        .child(
            div()
                .text_size(px(13.))
                .text_center()
                .max_w(px(320.))
                .child("Add a stdio command or an HTTP endpoint to start inspecting tools, resources and prompts."),
        )
        .child(
            h_flex()
                .gap(px(16.))
                .mt(px(12.))
                .child(
                    accent_button(cx, "Add server", 26.)
                        .id("empty-add-server")
                        .on_click(move |_, _, cx| {
                            entity.update(cx, |s, cx| s.show_add_server(cx));
                        }),
                )
                .child(kbd(cx, "⌘N")),
        )
}
