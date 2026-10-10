//! The update notice: one dim line on stderr after the command's output,
//! when a newer release is out.
//!
//! Only for a person at a terminal: nothing is checked or printed when
//! stderr is not one, under `CI`, or with the check turned off. The check
//! runs beside the command, on a thread of its own with a runtime of its
//! own, which the process leaves behind when it exits: the command's runtime
//! waits for its blocking tasks when dropped, and a name lookup is one, so a
//! resolver that does not answer would hold the exit for as long as the
//! system's own timeout. A command that ends first gives the check at most
//! [`GRACE`] more, on the one run a day that makes one, so a quick command
//! still gets an answer; the line comes from what the file holds then, and
//! a check that did not finish shows on a later run.

use std::io::IsTerminal as _;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use coco_update::{Channel, State};

use crate::style;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The longest a command that ended waits for a check still under way.
const GRACE: Duration = Duration::from_millis(500);

/// Where the notice is read from once the command ends.
#[derive(Debug)]
pub(crate) struct Pending {
    path: PathBuf,
    /// Hears when the check started for this run ends.
    done: Option<mpsc::Receiver<()>>,
}

/// Start a check if one is due and wanted; `None` when this run says
/// nothing about updates.
pub(crate) fn start() -> Option<Pending> {
    let wanted = std::io::stderr().is_terminal()
        && std::env::var_os("CI").is_none()
        && !coco_update::disabled_by_env(|name| std::env::var(name).ok());
    if !wanted {
        return None;
    }
    let path = State::default_path()?;
    let state = State::load(&path);
    if state.off {
        return None;
    }
    let mut done = None;
    if state.due(coco_update::now()) {
        let path = path.clone();
        let (tx, rx) = mpsc::channel();
        done = Some(rx);
        // Detached: never joined, and gone when the process exits.
        let _ = std::thread::Builder::new()
            .name("update-check".into())
            .spawn(move || {
                let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    return;
                };
                runtime.block_on(async {
                    let Ok(client) = coco_update::client(VERSION) else {
                        return;
                    };
                    // A failed check says nothing; it is tried again tomorrow.
                    let _ = coco_update::check(&client, coco_update::RELEASES_API, &path).await;
                });
                let _ = tx.send(());
            });
    }
    Some(Pending { path, done })
}

impl Pending {
    /// Print the notice, if there is one now.
    pub(crate) fn finish(self) {
        if let Some(done) = self.done {
            let _ = done.recv_timeout(GRACE);
        }
        let notice = State::load(&self.path).notice(VERSION, Channel::detect(), coco_update::now());
        if let Some(notice) = notice {
            anstream::eprintln!(
                "{}",
                style::paint(style::MUTED, style::plain(notice.line()))
            );
        }
    }
}
