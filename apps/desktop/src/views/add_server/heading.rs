//! The form's title, subtitle and button: what saving or connecting will
//! do, from the server the form edits as it is now.

use super::*;

/// The server the form edits, as the heading reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Server {
    /// A new one: the form adds it.
    Adding,
    /// Not connected, nothing failed.
    Idle,
    /// Connecting now.
    Connecting,
    /// In session: saving ends the session and starts another.
    Live,
    /// The last connect failed.
    Failed,
    /// The last connect was turned away for want of credentials; `oauth`
    /// when a new sign-in (Authorize) is the way back.
    Refused { oauth: bool },
}

/// What the form says above its fields and on its button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Heading {
    pub(super) title: &'static str,
    pub(super) subtitle: &'static str,
    pub(super) action: &'static str,
}

/// The heading of a form `opened` on purpose (the edit screen, which can
/// be left), or shown as the pane of a server that is not connected.
pub(super) fn heading(server: Server, opened: bool) -> Heading {
    let (title, subtitle) = match (server, opened) {
        (Server::Adding, _) => (
            "Add server",
            "Saved to the sidebar. Connection is attempted immediately.",
        ),
        (Server::Failed | Server::Refused { .. }, true) => (
            "Edit server",
            "The connection failed. Change the settings and connect again.",
        ),
        (Server::Live, true) => (
            "Edit server",
            "Saving ends the session and connects with the new settings.",
        ),
        (Server::Idle | Server::Connecting, true) => {
            ("Edit server", "Saving connects with the new settings.")
        }
        (Server::Refused { oauth: true }, false) => (
            "Authorization required",
            "Authorize to sign in again, or change the settings first.",
        ),
        (Server::Refused { oauth: false }, false) => (
            "Authorization required",
            "Set the credentials and connect again.",
        ),
        (Server::Failed, false) => (
            "Connection failed",
            "Change the settings and connect again.",
        ),
        (Server::Connecting, false) => ("Connecting", "Connecting with these settings."),
        // A server in session is not shown as its settings; were it, it
        // would read as one that is not.
        (Server::Idle | Server::Live, false) => (
            "Disconnected",
            "Connect with these settings, or change them first.",
        ),
    };
    let action = if server == Server::Live && opened {
        "Save & reconnect"
    } else {
        "Connect"
    };
    Heading {
        title,
        subtitle,
        action,
    }
}

impl AddServerForm {
    /// How the edited server stands now, and the failure of its last
    /// connect, which the form writes under its fields. Read from the
    /// server on every draw: it changes under the form (the row's plug, a
    /// connect finishing). The auth is the saved spec's, not the one the
    /// form opened with: a save that changed it is what the last connect
    /// tried.
    pub(super) fn standing(&self, cx: &App) -> (Server, Option<String>) {
        let Some(entry) = self
            .edited(cx)
            .and_then(|ix| self.state.read(cx).servers.get(ix))
        else {
            // Adding, or the server was deleted while the form was open.
            let server = if self.is_editing() {
                Server::Idle
            } else {
                Server::Adding
            };
            return (server, None);
        };
        let oauth = matches!(
            entry.record.spec,
            ServerSpec::Http {
                auth: AuthRef::OAuth { .. },
                ..
            }
        );
        match &entry.status {
            Status::Error(text) if entry.unauthorized => {
                (Server::Refused { oauth }, Some(text.clone()))
            }
            Status::Error(text) => (Server::Failed, Some(text.clone())),
            Status::Connected => (Server::Live, None),
            Status::Connecting => (Server::Connecting, None),
            Status::Off => (Server::Idle, None),
        }
    }

    /// The server whose failed connect this form writes under its fields
    /// now; `None` while it shows its own error instead, or no failure.
    pub fn failure_shown(&self, cx: &App) -> Option<usize> {
        let ix = self.edited(cx)?;
        let failed = matches!(self.state.read(cx).servers[ix].status, Status::Error(_));
        (failed && self.error.is_none()).then_some(ix)
    }
}

#[cfg(test)]
mod tests {
    use super::{Server, heading};

    fn read(server: Server, opened: bool) -> (&'static str, &'static str) {
        let h = heading(server, opened);
        (h.title, h.action)
    }

    #[test]
    fn the_edit_screen_says_what_saving_does() {
        assert_eq!(read(Server::Adding, true), ("Add server", "Connect"));
        assert_eq!(read(Server::Idle, true), ("Edit server", "Connect"));
        assert_eq!(
            read(Server::Live, true),
            ("Edit server", "Save & reconnect")
        );
        assert_eq!(
            heading(Server::Live, true).subtitle,
            "Saving ends the session and connects with the new settings."
        );
        assert_eq!(read(Server::Failed, true), ("Edit server", "Connect"));
        assert_eq!(
            read(Server::Refused { oauth: true }, true),
            ("Edit server", "Connect")
        );
    }

    #[test]
    fn the_pane_says_how_the_server_stands() {
        assert_eq!(read(Server::Idle, false), ("Disconnected", "Connect"));
        assert_eq!(read(Server::Connecting, false), ("Connecting", "Connect"));
        assert_eq!(
            read(Server::Failed, false),
            ("Connection failed", "Connect")
        );
        assert_eq!(
            heading(Server::Refused { oauth: true }, false).subtitle,
            "Authorize to sign in again, or change the settings first."
        );
        assert_eq!(
            heading(Server::Refused { oauth: false }, false).subtitle,
            "Set the credentials and connect again."
        );
        assert_eq!(
            read(Server::Refused { oauth: false }, false).0,
            "Authorization required"
        );
    }
}
