//! The times and durations the window shows, read through one place.
//!
//! Every time of day, date and duration on screen is formatted from a value
//! passed through here first. Normally the value is passed on as it is. The
//! headless renders call [`hold_still`], after which every time reads as
//! [`STILL_AT`] in UTC and every duration as [`STILL_FOR`]: a render that
//! showed the clock would differ on every run and on every machine, and
//! could not be compared with its reference in `docs/screenshots/`. The
//! commit a build is stamped with changes the same way, so it reads as
//! [`STILL_COMMIT`] once held, dated [`STILL_AT`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use time::{OffsetDateTime, UtcOffset};

/// The time every time reads as once held: noon UTC on the day the
/// 2026-07-28 protocol revision is named after.
pub const STILL_AT: OffsetDateTime = time::macros::datetime!(2026-07-28 12:00:00 UTC);

/// The duration every duration reads as once held.
pub const STILL_FOR: Duration = Duration::from_millis(42);

/// The commit every build reads as once held.
pub const STILL_COMMIT: &str = "0000000";

static STILL: AtomicBool = AtomicBool::new(false);

/// Show every time as [`STILL_AT`] and every duration as [`STILL_FOR`]
/// from now on, in this process. For the headless renders only: the window
/// a person uses always shows the real clock.
pub fn hold_still() {
    STILL.store(true, Ordering::Relaxed);
}

fn still() -> bool {
    STILL.load(Ordering::Relaxed)
}

/// `at`, as it is to be shown.
pub(crate) fn at(at: OffsetDateTime) -> OffsetDateTime {
    if still() { STILL_AT } else { at }
}

/// The offset times are shown in: the local one, or UTC once held, so a
/// render reads the same in every time zone.
pub(crate) fn offset() -> UtcOffset {
    if still() {
        UtcOffset::UTC
    } else {
        UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC)
    }
}

/// `elapsed`, as it is to be shown.
pub(crate) fn elapsed(elapsed: Duration) -> Duration {
    if still() { STILL_FOR } else { elapsed }
}

/// The short hash of the commit the build came from, as it is to be shown.
pub(crate) fn commit(commit: Option<&'static str>) -> Option<&'static str> {
    if still() { Some(STILL_COMMIT) } else { commit }
}

/// The build's commit date, as it is to be shown: `date` as git wrote it,
/// or [`STILL_AT`] in the same form once held.
pub(crate) fn build_date(date: Option<&'static str>) -> Option<String> {
    if still() {
        STILL_AT
            .format(time::macros::format_description!(
                "[month repr:short] [day padding:none], [year]"
            ))
            .ok()
    } else {
        date.map(str::to_owned)
    }
}

/// `ms` whole milliseconds, as they are to be shown: `<1 ms` or `N ms`.
pub(crate) fn millis(ms: u64) -> String {
    let ms = if still() {
        u64::try_from(STILL_FOR.as_millis()).unwrap_or(u64::MAX)
    } else {
        ms
    };
    if ms == 0 {
        "<1 ms".to_owned()
    } else {
        format!("{ms} ms")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_milliseconds_read_as_a_duration() {
        // The process-wide hold is left alone: other tests read the clock.
        assert_eq!(millis(0), "<1 ms");
        assert_eq!(millis(142), "142 ms");
    }
}
