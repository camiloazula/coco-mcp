//! The Settings section of the Server view: how the server is reached, and
//! the way into the form that changes it. Pressing its row in the list opens
//! the form at once; this is what shows when the row is reached another way,
//! such as with the keyboard or after leaving the form.

use super::*;

pub(super) fn settings(ws: &Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let t = *tokens(cx);
    let Some((name, target, protocol)) = ws.state.read(cx).server().map(|server| {
        (
            server.record.name.clone(),
            server.record.spec.label(),
            server.record.protocol,
        )
    }) else {
        return div().into_any_element();
    };
    let row = |label: &'static str, value: String| {
        h_flex()
            .gap(px(16.))
            .child(muted(cx, 12., label).w(px(140.)).flex_none())
            .child(mono(cx, 12., value).min_w_0().text_color(t.fg))
    };
    let state = ws.state.clone();
    let edit = accent_button(cx, "Edit server", 24.)
        .id("edit-server-settings")
        .on_click(move |_, _, cx| state.update(cx, |s, cx| s.show_edit_selected(cx)))
        .test_support();
    let body = v_flex()
        .gap(px(6.))
        .max_w(px(640.))
        .child(row("Name", name))
        .child(row("Connects to", target))
        .child(row("Protocol", protocol.label().to_owned()))
        .child(h_flex().pt(px(4.)).child(edit))
        .into_any_element();
    div()
        .px(px(24.))
        .py(px(8.))
        .child(labelled(cx, "Settings", div(), body))
        .into_any_element()
}
