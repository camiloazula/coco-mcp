//! The snapshots stored for a server, and the pickers that compare two.

use super::*;

/// The snapshots stored for the server, two of them picked for comparison,
/// and their diff in the change banner's terms.
pub(super) fn snapshots(ws: &mut Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let t = *tokens(cx);
    let (listed, pick, diff) = {
        let state = ws.state.read(cx);
        let Some(server) = state.server() else {
            return div().into_any_element();
        };
        (
            server.snapshots.clone().unwrap_or_default(),
            server.snapshot_pick.clone(),
            server.snapshot_diff.clone(),
        )
    };
    let rows: Vec<AnyElement> = listed
        .iter()
        .enumerate()
        .map(|(i, stored)| {
            let from = pick.0.as_deref() == Some(stored.id.as_str());
            let to = pick.1.as_deref() == Some(stored.id.as_str());
            let digest: String = stored.digest.chars().take(12).collect();
            h_flex()
                .id(("snapshot", i))
                .gap(px(16.))
                .items_center()
                .child(
                    mono(cx, 12., when_label(stored.taken_at))
                        .w(px(160.))
                        .flex_none()
                        .text_color(t.fg),
                )
                .child(mono(cx, 11., digest).flex_1().text_color(t.muted))
                .child(pick_button(
                    cx,
                    "From",
                    from,
                    ("snap-from", i),
                    stored.id.clone(),
                    false,
                ))
                .child(pick_button(
                    cx,
                    "To",
                    to,
                    ("snap-to", i),
                    stored.id.clone(),
                    true,
                ))
                .test_support()
                .into_any_element()
        })
        .collect();
    let comparison: AnyElement = match diff {
        Some(Ok(diff)) => {
            let color = if diff.has_breaking() { t.err } else { t.accent };
            let changes: Vec<AnyElement> = diff
                .changes()
                .iter()
                .enumerate()
                .map(|(i, change)| banner::change_row(ws, i, change, cx))
                .collect();
            v_flex()
                .gap(px(4.))
                .child(
                    mono(cx, 11., diff.summary())
                        .id("snapshot-summary")
                        .text_color(color)
                        .test_support(),
                )
                .children(changes)
                .into_any_element()
        }
        Some(Err(e)) => div()
            .text_size(px(12.))
            .text_color(t.err)
            .child(e)
            .into_any_element(),
        None if listed.len() < 2 => muted(
            cx,
            12.,
            "Two stored snapshots are needed to compare; one is stored whenever a \
             connect finds the server changed.",
        )
        .into_any_element(),
        None => muted(cx, 12., "Comparing…").into_any_element(),
    };
    let compare_file = text_tab(cx, "Compare with a file…", false)
        .id("compare-file")
        .on_click(cx.listener(|ws, _, _, cx| ws.compare_snapshot_file(cx)))
        .test_support();
    let body = v_flex()
        .gap(px(8.))
        .child(v_flex().gap(px(4.)).children(rows))
        .child(comparison)
        .child(h_flex().child(compare_file))
        .into_any_element();
    div()
        .px(px(24.))
        .py(px(8.))
        .child(labelled(cx, "Snapshots", div(), body))
        .into_any_element()
}

/// A `from` or `to` pick of one stored snapshot.
pub(super) fn pick_button(
    cx: &mut Context<Workspace>,
    label: &'static str,
    picked: bool,
    id: (&'static str, usize),
    snapshot: String,
    newer: bool,
) -> AnyElement {
    text_tab(cx, label, picked)
        .id(id)
        .on_click(cx.listener(move |ws, _, _, cx| {
            let snapshot = snapshot.clone();
            ws.state
                .update(cx, |s, cx| s.pick_snapshot(snapshot, newer, cx));
        }))
        .test_support()
        .into_any_element()
}
