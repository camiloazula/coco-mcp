//! What the last check found and what the user chose, in `update.json`.
//!
//! The window and the command line read and write the same file. Each write
//! reads it again first and changes only its own fields, so a check that
//! finishes in one does not undo a version skipped in the other meanwhile.
//! A file that is missing or cannot be read is a state with nothing in it:
//! the worst that costs is one more check.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use semver::Version;
use serde::{Deserialize, Serialize};

use crate::Channel;

/// The file's name, in the data directory beside `coco.db`.
const FILE: &str = "update.json";

/// A stable release, as the check found it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    /// `0.5.0`, without the tag's `v`.
    pub version: String,
    /// When it was published, as Unix seconds.
    pub published_at: i64,
    /// Its page, with the notes.
    pub url: String,
}

/// What the last check found, and what the user chose.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// When the last check ended, answered or not, as Unix seconds.
    pub checked_at: Option<i64>,
    /// The `ETag` of the last answer, sent back with the next check.
    pub etag: Option<String>,
    /// The latest release the last answer named.
    pub latest: Option<Release>,
    /// A version the user asked not to hear about again.
    pub skipped: Option<String>,
    /// Checks turned off from the window.
    pub off: bool,
}

/// What to tell the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    /// The newer release.
    pub latest: Release,
    /// The version running.
    pub current: String,
    /// The command that upgrades this install, if there is one.
    pub command: Option<&'static str>,
}

impl Notice {
    /// `coco-mcp 0.5.0 is available (you have 0.4.0): brew upgrade coco-mcp`,
    /// the line the command line prints; the release page stands in for a
    /// command when there is none.
    pub fn line(&self) -> String {
        format!(
            "coco-mcp {} is available (you have {}): {}",
            self.latest.version,
            self.current,
            self.command.unwrap_or(&self.latest.url)
        )
    }
}

impl State {
    /// `update.json` in the data directory, where the database lives.
    pub fn default_path() -> Option<PathBuf> {
        directories::ProjectDirs::from("", "", "coco-mcp").map(|dirs| dirs.data_dir().join(FILE))
    }

    /// The state in `path`; empty when there is none or it cannot be read.
    pub fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Read `path` again, apply `change`, and write it back whole: written
    /// beside it and renamed over it, so a reader never sees half a file.
    /// Returns the state written.
    ///
    /// Writes within one process take turns, so a check landing as the user
    /// skips a version keeps both. Between processes (the window and a
    /// command line) the read is fresh but nothing is locked: two writes in
    /// the same instant can lose one field, which costs at most one check or
    /// one more notice.
    pub fn update(path: &Path, change: impl FnOnce(&mut Self)) -> std::io::Result<Self> {
        static TURN: Mutex<()> = Mutex::new(());
        static PARTIALS: AtomicU64 = AtomicU64::new(0);
        let _turn = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let mut state = Self::load(path);
        change(&mut state);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let bytes = serde_json::to_vec_pretty(&state).map_err(std::io::Error::other)?;
        let partial = path.with_extension(format!(
            "json.{}.{}",
            std::process::id(),
            PARTIALS.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&partial, bytes)?;
        if let Err(error) = std::fs::rename(&partial, path) {
            let _ = std::fs::remove_file(&partial);
            return Err(error);
        }
        Ok(state)
    }

    /// Whether a check is due at `now`: never checked, or not for
    /// [`crate::INTERVAL`]. A clock set back past the last check makes one
    /// due too, rather than none for as long.
    pub fn due(&self, now: i64) -> bool {
        let interval = i64::try_from(crate::INTERVAL.as_secs()).unwrap_or(i64::MAX);
        self.checked_at
            .is_none_or(|at| now < at || now.saturating_sub(at) >= interval)
    }

    /// Whether the last check found nothing newer than `current`.
    pub fn up_to_date(&self, current: &str) -> bool {
        self.checked_at.is_some() && self.newer(current).is_none()
    }

    /// The latest release, when it is newer than `current`, skipped or not.
    /// A version that does not parse is never newer.
    pub fn newer(&self, current: &str) -> Option<&Release> {
        let latest = self.latest.as_ref()?;
        let theirs = Version::parse(&latest.version).ok()?;
        let ours = Version::parse(current).ok()?;
        (theirs.pre.is_empty() && theirs > ours).then_some(latest)
    }

    /// What to tell the user at `now`, running `current` installed through
    /// `channel`: a newer stable release that is not skipped, once the
    /// install's packages have had [`crate::PACKAGE_LAG`] to follow it.
    pub fn notice(&self, current: &str, channel: Channel, now: i64) -> Option<Notice> {
        if self.off {
            return None;
        }
        let latest = self.newer(current)?;
        if self.skipped.as_deref() == Some(latest.version.as_str()) {
            return None;
        }
        let lag = i64::try_from(crate::PACKAGE_LAG.as_secs()).unwrap_or(i64::MAX);
        if channel.lags() && now.saturating_sub(latest.published_at) < lag {
            return None;
        }
        Some(Notice {
            latest: latest.clone(),
            current: current.to_owned(),
            command: channel.command(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Manager, Scope};

    const HOUR: i64 = 60 * 60;
    const PUBLISHED: i64 = 1_791_000_000;

    fn found(version: &str) -> State {
        State {
            checked_at: Some(PUBLISHED),
            latest: Some(Release {
                version: version.to_owned(),
                published_at: PUBLISHED,
                url: format!("https://github.com/camiloazula/coco-mcp/releases/tag/v{version}"),
            }),
            ..State::default()
        }
    }

    #[test]
    fn a_newer_release_is_announced_with_the_install_command() {
        let notice = found("0.5.0")
            .notice("0.4.0", Channel::Homebrew, PUBLISHED + 3 * HOUR)
            .unwrap();
        assert_eq!(notice.latest.version, "0.5.0");
        assert_eq!(
            notice.line(),
            "coco-mcp 0.5.0 is available (you have 0.4.0): brew upgrade coco-mcp"
        );
    }

    #[test]
    fn an_archive_is_pointed_at_the_release_page() {
        let notice = found("0.5.0")
            .notice("0.4.0", Channel::Archive, PUBLISHED)
            .unwrap();
        assert_eq!(
            notice.line(),
            "coco-mcp 0.5.0 is available (you have 0.4.0): \
             https://github.com/camiloazula/coco-mcp/releases/tag/v0.5.0"
        );
    }

    #[test]
    fn the_same_an_older_or_a_prerelease_says_nothing() {
        for (latest, current) in [
            ("0.4.0", "0.4.0"),
            ("0.3.1", "0.4.0"),
            ("0.5.0-rc.1", "0.4.0"),
            ("not a version", "0.4.0"),
        ] {
            assert_eq!(
                found(latest).notice(current, Channel::Archive, PUBLISHED),
                None,
                "{latest} over {current}"
            );
        }
        assert!(found("0.4.0").up_to_date("0.4.0"));
        assert!(!found("0.5.0").up_to_date("0.4.0"));
        assert!(!State::default().up_to_date("0.4.0"), "never checked");
    }

    #[test]
    fn versions_compare_as_numbers() {
        assert!(
            found("0.10.0")
                .notice("0.9.3", Channel::Archive, PUBLISHED)
                .is_some()
        );
        assert!(
            found("1.0.0")
                .notice("1.0.0-beta.2", Channel::Archive, PUBLISHED)
                .is_some()
        );
    }

    #[test]
    fn packages_get_two_hours_to_follow_the_release() {
        let state = found("0.5.0");
        let npm = Channel::Js(Manager::Npm, Scope::Global);
        assert_eq!(state.notice("0.4.0", npm, PUBLISHED + HOUR), None);
        assert!(state.notice("0.4.0", npm, PUBLISHED + 2 * HOUR).is_some());
        assert!(
            state.notice("0.4.0", Channel::Archive, PUBLISHED).is_some(),
            "an archive is there as soon as the release is"
        );
    }

    #[test]
    fn a_skipped_version_is_quiet_until_a_newer_one() {
        let mut state = found("0.5.0");
        state.skipped = Some("0.5.0".into());
        assert_eq!(state.notice("0.4.0", Channel::Archive, PUBLISHED), None);
        state.latest = found("0.5.1").latest;
        assert!(state.notice("0.4.0", Channel::Archive, PUBLISHED).is_some());
    }

    #[test]
    fn turned_off_says_nothing() {
        let mut state = found("0.5.0");
        state.off = true;
        assert_eq!(state.notice("0.4.0", Channel::Archive, PUBLISHED), None);
    }

    #[test]
    fn a_check_is_due_daily() {
        let mut state = State::default();
        assert!(state.due(PUBLISHED), "never checked");
        state.checked_at = Some(PUBLISHED);
        assert!(!state.due(PUBLISHED + 23 * HOUR));
        assert!(state.due(PUBLISHED + 24 * HOUR));
        assert!(state.due(PUBLISHED - HOUR), "the clock went back");
    }

    #[test]
    fn the_file_round_trips_and_keeps_what_another_process_wrote() {
        let dir = std::env::temp_dir().join(format!("coco-update-file-{}", std::process::id()));
        let path = dir.join("nested").join(FILE);
        assert_eq!(State::load(&path), State::default(), "missing");

        State::update(&path, |s| s.skipped = Some("0.5.0".into())).unwrap();
        let written = State::update(&path, |s| s.checked_at = Some(PUBLISHED)).unwrap();
        assert_eq!(written.skipped.as_deref(), Some("0.5.0"), "kept");
        assert_eq!(State::load(&path), written);

        std::fs::write(&path, b"{ not json").unwrap();
        assert_eq!(State::load(&path), State::default(), "unreadable");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn writes_from_many_threads_keep_every_field() {
        let dir = std::env::temp_dir().join(format!("coco-update-threads-{}", std::process::id()));
        let path = dir.join(FILE);
        let threads: Vec<_> = (0..16)
            .map(|i| {
                let path = path.clone();
                std::thread::spawn(move || {
                    if i == 7 {
                        State::update(&path, |s| s.skipped = Some("0.5.0".into())).unwrap();
                    } else {
                        State::update(&path, |s| s.checked_at = Some(i)).unwrap();
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        let state = State::load(&path);
        assert_eq!(state.skipped.as_deref(), Some("0.5.0"));
        assert!(state.checked_at.is_some());
        let leftovers = std::fs::read_dir(&dir).unwrap().count();
        assert_eq!(leftovers, 1, "no partial file left behind");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn fields_it_does_not_know_are_ignored() {
        let state: State =
            serde_json::from_str(r#"{"checked_at": 5, "later_field": true}"#).unwrap();
        assert_eq!(state.checked_at, Some(5));
    }
}
