//! Whether a newer Coco MCP release is out, and how this install gets it.
//!
//! The app and the command line both say when a newer release exists. They
//! only say it: nothing here downloads or replaces the binary, which would
//! fight Homebrew and npm over the files they own.
//!
//! - [`check`] asks GitHub for the latest release, at most once a day
//!   ([`INTERVAL`]), with the `ETag` of the last answer so an unchanged one
//!   is a 304.
//! - [`State`] is what the last check found, plus the user's choices (a
//!   skipped version, checks turned off), kept in `update.json` next to the
//!   database so the window and the command line share it.
//! - [`Channel`] is how this copy was installed, read from `COCO_INSTALL`
//!   (set by the npm shim) or the executable's path, and gives the command
//!   that upgrades it.
//! - [`State::notice`] puts the three together: what to show, if anything.
//!
//! The request carries a `User-Agent` naming the version and nothing else;
//! [`disabled_by_env`] and [`State::off`] stop it altogether.
//!
//! UI-free: must never depend on `gpui` or `gpui-kit`.

#![forbid(unsafe_code)]
// unwrap()/expect() are denied in shipped code but fine inside tests.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod channel;
mod check;
mod state;

use std::time::Duration;

pub use channel::{Channel, Manager, Scope};
pub use check::{Error, check, client};
pub use state::{Notice, Release, State};

/// The latest release of the main repository; drafts and prereleases are
/// never this one.
pub const RELEASES_API: &str = "https://api.github.com/repos/camiloazula/coco-mcp/releases/latest";

/// Where every release is listed, for an install with no command to give.
pub const RELEASES_PAGE: &str = "https://github.com/camiloazula/coco-mcp/releases";

/// The shortest time between two checks made on their own.
pub const INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// How long a check may take before it is given up.
pub const TIMEOUT: Duration = Duration::from_secs(3);

/// How long after a release Homebrew and npm are trusted to have it. The
/// formula and the packages follow a release by up to an hour; until they
/// do, `brew upgrade` and `@latest` would install the version already here.
pub const PACKAGE_LAG: Duration = Duration::from_secs(2 * 60 * 60);

/// Set to anything but `0` or `false` to stop the check.
pub const NO_CHECK_VAR: &str = "COCO_NO_UPDATE_CHECK";

/// Whether the environment turns the check off: [`NO_CHECK_VAR`], or
/// `DO_NOT_TRACK` (consoledonottrack.com), either set to anything but `0`
/// or `false`.
pub fn disabled_by_env(var: impl Fn(&str) -> Option<String>) -> bool {
    [NO_CHECK_VAR, "DO_NOT_TRACK"].iter().any(|name| {
        var(name).is_some_and(|value| {
            let value = value.trim();
            !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false")
        })
    })
}

/// The time now, as Unix seconds: the unit [`State`] keeps.
pub fn now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        }
    }

    #[test]
    fn either_variable_turns_the_check_off() {
        assert!(!disabled_by_env(env(&[])));
        assert!(disabled_by_env(env(&[("COCO_NO_UPDATE_CHECK", "1")])));
        assert!(disabled_by_env(env(&[("DO_NOT_TRACK", "1")])));
        assert!(disabled_by_env(env(&[("DO_NOT_TRACK", "yes")])));
    }

    #[test]
    fn zero_false_and_empty_leave_it_on() {
        for value in ["0", "false", "FALSE", "", " "] {
            assert!(
                !disabled_by_env(env(&[("COCO_NO_UPDATE_CHECK", value)])),
                "{value:?}"
            );
            assert!(
                !disabled_by_env(env(&[("DO_NOT_TRACK", value)])),
                "{value:?}"
            );
        }
    }
}
