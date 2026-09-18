//! One log row, collapsed or expanded.

use super::*;

/// Row `ix` of the log of the server with id `server`, which names the row
/// to its folds and to its menu.
pub(super) fn row(
    ix: usize,
    row: &LogRow,
    expanded: bool,
    server: &SharedString,
    ws: &Workspace,
    cx: &Context<Workspace>,
) -> AnyElement {
    let t = *tokens(cx);
    let entity = ws.state.clone();
    let arrow_color = match row.dir {
        Dir::Out => t.accent,
        Dir::In => t.fg,
        Dir::Note => t.muted,
    };
    // Keyed by the row's id, not its index: the cap shifts indices.
    let id = row.row_id;
    let line = h_flex()
        .id(("log-row", ix))
        .w_full()
        .h(px(24.))
        .px(px(12.))
        .gap(px(12.))
        .text_color(if row.is_error { t.err } else { t.fg })
        .when(expanded, |el| el.bg(t.sel))
        // A reconnect's separator: the rows above it are the previous session's.
        .when(row.session_break, |el| {
            el.border_t_1().border_color(t.hair).text_color(t.muted)
        })
        .hover(|s| s.bg(t.hover))
        .on_click(move |_, _, cx| {
            entity.update(cx, |s, cx| s.toggle_log_row(id, cx));
        })
        .child(
            div()
                .w(px(90.))
                .flex_none()
                .text_color(t.muted)
                .child(row.time.clone()),
        )
        .child(
            div()
                .w(px(20.))
                .flex_none()
                .text_color(arrow_color)
                .child(row.dir.arrow()),
        )
        .child(
            div()
                .w(px(200.))
                .flex_none()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .child(row.method.clone()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_color(t.muted)
                .child(row.body.clone()),
        )
        .child(
            div()
                .w(px(60.))
                .flex_none()
                .text_right()
                .text_color(t.muted)
                .child(row.size.clone()),
        )
        .test_support();
    let state = ws.state.clone();
    let server_id = server.clone();
    let line = line
        .on_mouse_down(MouseButton::Right, move |event, _, cx| {
            // A row that left the log since this frame offers nothing.
            let entries = state
                .read(cx)
                .log_row_by_id(&server_id, id)
                .map(|(row, spec)| row_entries(row, spec));
            if let Some(entries) = entries {
                clip::open_menu(entries, event.position, cx);
            }
        })
        .into_any_element();
    if expanded {
        let toggle = ws.collapse_toggle(cx);
        v_flex()
            .w_full()
            .child(line)
            .child(div().px(px(12.)).pl(px(134.)).py(px(6.)).child(json_tree(
                &row.payload,
                &ws.folds(&toggle),
                &fold_prefix(server, id),
                cx,
            )))
            .into_any_element()
    } else {
        line
    }
}
