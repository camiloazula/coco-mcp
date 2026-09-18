//! A response's labels: elapsed time, size and shape, and the prompt to
//! show a held result.

use super::*;

/// The clock of a request still waiting, in tenths of a second.
pub(super) fn running_label(elapsed: Duration) -> String {
    format!("{:.1} s", elapsed.as_secs_f64())
}

/// `0.4 ms` under ten milliseconds, whole milliseconds above.
pub fn elapsed_label(elapsed: std::time::Duration) -> String {
    let ms = elapsed.as_secs_f64() * 1000.0;
    if ms < 10.0 {
        format!("{ms:.1} ms")
    } else {
        format!("{} ms", ms.round() as u64)
    }
}

/// The header's facts: method and status, and for a result held back, its
/// size and top-level shape, which is all that is known without drawing it.
pub(super) fn meta(shown: &Shown<'_>, held: bool) -> String {
    let status = match &shown.status {
        ResponseStatus::Pending => "Pending",
        ResponseStatus::Ok => "OK",
        ResponseStatus::ToolError => "isError",
        ResponseStatus::Cancelled => "Cancelled",
        ResponseStatus::Failed(_) => "Failed",
    };
    if held {
        format!(
            "{} · {status} · {} · {}",
            shown.method,
            state::size_label(shown.size),
            shape(&shown.raw)
        )
    } else {
        format!("{} · {status}", shown.method)
    }
}

/// The top level of `value` in a few words, read without walking it.
pub(super) fn shape(value: &Value) -> String {
    match value {
        Value::Object(map) => format!("Object · {} keys", map.len()),
        Value::Array(items) => format!("Array · {} items", items.len()),
        Value::String(_) => "String".to_owned(),
        Value::Number(_) => "Number".to_owned(),
        Value::Bool(_) => "Boolean".to_owned(),
        Value::Null => "Null".to_owned(),
    }
}

/// In place of a held-back result: why it is not drawn, and the button that
/// draws it. The choice is kept for the selection, so a re-run stays shown.
pub(super) fn show_result(prefix: &str, cx: &Context<Workspace>) -> AnyElement {
    let key = prefix.to_owned();
    h_flex()
        .gap(px(12.))
        .items_center()
        .child(muted(
            cx,
            12.,
            "Not drawn yet: a result this large takes a moment to lay out.",
        ))
        .child(
            accent_button(cx, "Show result", 24.)
                .id(SharedString::from(format!("{prefix}-reveal")))
                .on_click(cx.listener(move |ws, _, _, cx| {
                    ws.revealed.insert(key.clone());
                    cx.notify();
                }))
                .test_support(),
        )
        .into_any_element()
}
