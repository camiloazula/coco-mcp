//! The update notice in the window: what the last check found, the check
//! run at launch and by hand, and the user's answers to it.
//!
//! A global rather than workspace state, like the copy menu: the status bar,
//! its popover and the About window, a window of its own, all read it. The
//! check and every write of `update.json` run on the runtime, never on the
//! GPUI thread; the file and the rules live in `coco-update`, which the
//! command line shares.

use std::path::PathBuf;

use coco_update::{Channel, Notice, State};
use gpui_kit::{App, Global};

use crate::bridge::Bridge;

/// The version running.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// What the window knows about updates.
#[derive(Debug)]
pub struct Update {
    bridge: Bridge,
    path: Option<PathBuf>,
    channel: Channel,
    state: State,
    /// Checks are off in the environment (`COCO_NO_UPDATE_CHECK`,
    /// `DO_NOT_TRACK`); only a check asked for by hand is made.
    env_off: bool,
    checking: bool,
    /// A check was asked for by hand while one was under way: say how the
    /// one under way ends.
    report: bool,
    /// The popover is open.
    pub open: bool,
    /// Closed for this launch.
    dismissed: bool,
}

impl Global for Update {}

impl Update {
    /// A model over `state`, for the headless renders: no file, no check.
    pub fn fixed(bridge: Bridge, channel: Channel, state: State) -> Self {
        Self {
            bridge,
            path: None,
            channel,
            state,
            env_off: true,
            checking: false,
            report: false,
            open: false,
            dismissed: false,
        }
    }

    /// The same model with the environment's say changed, for the renders
    /// of the Settings window, which draws a choice the environment decided
    /// differently from one the person may make.
    pub fn with_env_off(mut self, env_off: bool) -> Self {
        self.env_off = env_off;
        self
    }

    /// What to show in the status bar and the popover, if anything.
    pub fn notice(&self) -> Option<Notice> {
        if self.dismissed {
            return None;
        }
        self.state.notice(VERSION, self.channel, coco_update::now())
    }

    /// The About window's line, and the release page it opens: a newer
    /// release, even one skipped or closed, or that there is none. Nothing
    /// before a first check.
    pub fn about_line(&self) -> Option<(String, Option<String>)> {
        if let Some(latest) = self.state.newer(VERSION) {
            Some((
                format!("{} is available", latest.version),
                Some(latest.url.clone()),
            ))
        } else if self.state.up_to_date(VERSION) {
            Some(("Up to date".to_owned(), None))
        } else {
            None
        }
    }

    /// Whether checks are on: neither the environment nor the window's own
    /// setting turned them off.
    pub fn on(&self) -> bool {
        !self.state.off && !self.env_off
    }

    /// What a check asked for by hand found, at `now`. A newer release is
    /// named even when skipped or with checks off, since the user asked;
    /// one that Homebrew or npm has not caught up with yet says so, rather
    /// than offering a command that would install this version again.
    pub fn report(&self, now: i64) -> String {
        if let Some(notice) = self.state.notice(VERSION, self.channel, now) {
            return format!("{} is available", notice.latest.version);
        }
        let Some(latest) = self.state.newer(VERSION) else {
            return format!("{} is up to date", crate::APP_NAME);
        };
        let lag = i64::try_from(coco_update::PACKAGE_LAG.as_secs()).unwrap_or(i64::MAX);
        if self.channel.lags() && now.saturating_sub(latest.published_at) < lag {
            format!(
                "{} is out; your package manager will have it within two hours",
                latest.version
            )
        } else {
            format!("{} is available", latest.version)
        }
    }
}

/// Read what the last check found, and check if one is due. Called once at
/// launch.
pub fn start(bridge: Bridge, cx: &mut App) {
    let path = State::default_path();
    let env_off = coco_update::disabled_by_env(|name| std::env::var(name).ok());
    cx.set_global(Update {
        bridge: bridge.clone(),
        path: path.clone(),
        channel: Channel::detect(),
        state: State::default(),
        env_off,
        checking: false,
        report: false,
        open: false,
        dismissed: false,
    });
    let Some(path) = path else {
        return;
    };
    let read = bridge.run_blocking(move || State::load(&path));
    cx.spawn(async move |cx| {
        let Some(state) = read.await else {
            return;
        };
        cx.update(|cx| {
            let due = !env_off && !state.off && state.due(coco_update::now());
            set_state(state, cx);
            if due {
                check(false, cx);
            }
        });
    })
    .detach();
}

/// Ask GitHub now. `by_hand` says the outcome in the status bar, brings
/// back a note closed for this launch, and checks even where the environment
/// turned checks off: it was asked for.
pub fn check(by_hand: bool, cx: &mut App) {
    let Some(update) = cx.try_global::<Update>() else {
        return;
    };
    if update.checking {
        if by_hand {
            with(cx, |update| update.report = true);
            crate::clip::announce("checking for updates…", cx);
        }
        return;
    }
    let Some(path) = update.path.clone() else {
        if by_hand {
            crate::clip::announce("cannot check for updates: no data directory", cx);
        }
        return;
    };
    let run = update.bridge.run(async move {
        let client = coco_update::client(VERSION).map_err(|e| e.to_string())?;
        coco_update::check(&client, coco_update::RELEASES_API, &path)
            .await
            .map_err(|e| e.to_string())
    });
    with(cx, |update| {
        update.checking = true;
        update.report = by_hand;
    });
    if by_hand {
        crate::clip::announce("checking for updates…", cx);
    }
    cx.spawn(async move |cx| {
        let outcome = run.await;
        cx.update(|cx| {
            let mut report = false;
            with(cx, |update| {
                update.checking = false;
                report = std::mem::take(&mut update.report);
            });
            match outcome {
                Some(Ok(state)) => {
                    set_state(state, cx);
                    if report {
                        with(cx, |update| update.dismissed = false);
                        let note = cx.global::<Update>().report(coco_update::now());
                        crate::clip::announce(note, cx);
                    }
                }
                Some(Err(message)) => {
                    tracing::info!("update check: {message}");
                    if report {
                        crate::clip::announce(format!("cannot check for updates: {message}"), cx);
                    }
                }
                None => {}
            }
        });
    })
    .detach();
}

/// Open or close the popover.
pub fn toggle(cx: &mut App) {
    with(cx, |update| update.open = !update.open);
}

/// Close the popover, keeping the status bar's note.
pub fn close(cx: &mut App) {
    if cx.try_global::<Update>().is_some_and(|u| u.open) {
        with(cx, |update| update.open = false);
    }
}

/// Close the popover and the note until the next launch.
pub fn dismiss(cx: &mut App) {
    with(cx, |update| {
        update.open = false;
        update.dismissed = true;
    });
}

/// Stop announcing the release on offer until a newer one.
pub fn skip(cx: &mut App) {
    let Some(version) = cx
        .try_global::<Update>()
        .and_then(|u| u.state.latest.as_ref())
        .map(|latest| latest.version.clone())
    else {
        return;
    };
    change(move |state| state.skipped = Some(version.clone()), cx);
    close(cx);
}

/// Whether checks run, and whether the environment alone turned them off
/// (which the window cannot change). Without a model, off by the environment.
pub fn checks(cx: &App) -> (bool, bool) {
    cx.try_global::<Update>()
        .map_or((false, true), |u| (u.on(), u.env_off))
}

/// Turn checks on or off from the window, for the command line too.
pub fn toggle_checks(cx: &mut App) {
    let Some(update) = cx.try_global::<Update>() else {
        return;
    };
    if update.env_off {
        crate::clip::announce(
            "update checks are off in the environment (COCO_NO_UPDATE_CHECK or DO_NOT_TRACK)",
            cx,
        );
        return;
    }
    let on = update.on();
    change(move |state| state.off = on, cx);
    crate::clip::announce(
        if on {
            "update checks turned off"
        } else {
            "update checks turned on"
        },
        cx,
    );
    // Turned back on: check now if one is due.
    if !on
        && cx
            .try_global::<Update>()
            .is_some_and(|u| u.state.due(coco_update::now()))
    {
        check(false, cx);
    }
}

/// Apply `edit` at once, and to the file on the runtime.
fn change(edit: impl Fn(&mut State) + Send + 'static, cx: &mut App) {
    let Some(update) = cx.try_global::<Update>() else {
        return;
    };
    let (bridge, path) = (update.bridge.clone(), update.path.clone());
    with(cx, |update| edit(&mut update.state));
    let Some(path) = path else {
        return;
    };
    let write = bridge.run_blocking(move || State::update(&path, edit));
    cx.spawn(async move |_| {
        if let Some(Err(e)) = write.await {
            tracing::warn!("cannot write the update state: {e}");
        }
    })
    .detach();
}

fn set_state(state: State, cx: &mut App) {
    with(cx, |update| update.state = state);
}

/// Change the model, if there is one, and redraw every window that shows it.
fn with(cx: &mut App, change: impl FnOnce(&mut Update)) {
    if cx.has_global::<Update>() {
        change(cx.global_mut::<Update>());
        cx.refresh_windows();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coco_update::{Manager, Release, Scope};

    const PUBLISHED: i64 = 1_760_097_600;
    const HOUR: i64 = 60 * 60;

    fn found(channel: Channel, skipped: bool, off: bool) -> Update {
        let current = semver::Version::parse(VERSION).unwrap();
        let latest = format!("{}.{}.0", current.major, current.minor + 1);
        let state = State {
            checked_at: Some(PUBLISHED),
            skipped: skipped.then(|| latest.clone()),
            off,
            latest: Some(Release {
                version: latest,
                published_at: PUBLISHED,
                url: String::new(),
            }),
            ..State::default()
        };
        Update::fixed(Bridge::new().unwrap(), channel, state)
    }

    #[test]
    fn a_check_by_hand_names_a_release_even_skipped_or_turned_off() {
        let later = PUBLISHED + 3 * HOUR;
        for (skipped, off) in [(false, false), (true, false), (false, true)] {
            let report = found(Channel::Homebrew, skipped, off).report(later);
            assert!(report.ends_with(" is available"), "{report}");
        }
    }

    #[test]
    fn a_check_by_hand_says_when_the_package_has_not_caught_up() {
        let npm = Channel::Js(Manager::Npm, Scope::Global);
        let report = found(npm, false, false).report(PUBLISHED + HOUR);
        assert!(report.contains("within two hours"), "{report}");
        let report = found(Channel::Archive, false, false).report(PUBLISHED + HOUR);
        assert!(report.ends_with(" is available"), "{report}");
    }

    #[test]
    fn nothing_newer_is_up_to_date() {
        let update = Update::fixed(
            Bridge::new().unwrap(),
            Channel::Archive,
            State {
                checked_at: Some(PUBLISHED),
                ..State::default()
            },
        );
        assert_eq!(update.report(PUBLISHED), "Coco MCP is up to date");
    }
}
