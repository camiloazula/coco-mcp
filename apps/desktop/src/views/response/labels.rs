//! A response's labels: elapsed time, method and status.

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

/// The header's facts: method and status.
pub(super) fn meta(shown: &Shown<'_>) -> String {
    let status = match &shown.status {
        ResponseStatus::Pending => "Pending",
        ResponseStatus::Ok => "OK",
        ResponseStatus::ToolError => "isError",
        ResponseStatus::Cancelled => "Cancelled",
        ResponseStatus::Failed(_) => "Failed",
    };
    format!("{} · {status}", shown.method)
}
