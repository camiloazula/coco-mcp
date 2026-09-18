//! How to reach an MCP server. Persisted by `mcp-store`, consumed by `Session`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Reference to a stored credential. Secrets never live in this struct;
/// `mcp-auth` resolves the reference through the OS keyring.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuthRef {
    /// No authentication.
    None,
    /// `Authorization: Bearer <token>` where the token is stored under `keyring_id`.
    Bearer {
        /// Keyring entry holding the token.
        keyring_id: String,
    },
    /// OAuth 2.1 with PKCE and dynamic client registration.
    OAuth {
        /// Keyring entry holding the token set (access, refresh, expiry).
        keyring_id: String,
    },
}

/// Everything needed to connect to a server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "transport", rename_all = "snake_case")]
pub enum ServerSpec {
    /// Spawn a local process and speak JSON-RPC over its stdio.
    Stdio {
        /// Executable to run.
        command: String,
        /// Arguments passed verbatim.
        #[serde(default)]
        args: Vec<String>,
        /// Extra environment variables. Values must not be secrets.
        #[serde(default)]
        env: BTreeMap<String, String>,
        /// Working directory; inherits the app's when `None`.
        #[serde(default)]
        cwd: Option<PathBuf>,
    },
    /// Streamable HTTP transport.
    Http {
        /// Endpoint URL.
        url: String,
        /// Static headers sent with every request (non-secret).
        #[serde(default)]
        headers: BTreeMap<String, String>,
        /// Credential reference.
        #[serde(default = "AuthRef::none")]
        auth: AuthRef,
    },
}

impl AuthRef {
    const fn none() -> Self {
        Self::None
    }
}

/// Which protocol era a session speaks. Saved with each server, beside its
/// [`ServerSpec`] rather than inside it, since both transports take it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolMode {
    /// The `initialize` handshake, agreeing to 2025-11-25 or an older version
    /// the server answers with.
    #[default]
    Legacy,
    /// Ask with `server/discover` for 2026-07-28 and fall back to the
    /// handshake when the server does not speak it.
    Auto,
    /// 2026-07-28 only, without a handshake.
    Modern,
}

impl ProtocolMode {
    /// All modes, in the order the server form offers them.
    pub const ALL: [Self; 3] = [Self::Legacy, Self::Auto, Self::Modern];

    /// Its name in the database and on the command line.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Legacy => "legacy",
            Self::Auto => "auto",
            Self::Modern => "modern",
        }
    }

    /// Its label in the app.
    pub fn label(self) -> &'static str {
        match self {
            Self::Legacy => "Legacy",
            Self::Auto => "Auto",
            Self::Modern => "Modern",
        }
    }

    /// Parse [`Self::as_str`] (case-insensitive).
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|mode| mode.as_str().eq_ignore_ascii_case(s.trim()))
    }
}

impl ServerSpec {
    /// Short human label used in lists and logs. A stdio command is quoted
    /// the way the server form shows it, so two argument lists never share
    /// a label.
    pub fn label(&self) -> String {
        match self {
            Self::Stdio { command, args, .. } => crate::command_line(
                std::iter::once(command.as_str()).chain(args.iter().map(String::as_str)),
            ),
            Self::Http { url, .. } => url.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stdio_spec_round_trips_through_json() {
        let spec = ServerSpec::Stdio {
            command: "srv".into(),
            args: vec!["--verbose".into(), "filesystem-server".into()],
            env: BTreeMap::new(),
            cwd: None,
        };
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(json["transport"], "stdio");
        let back: ServerSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, spec);
    }

    #[test]
    fn http_spec_defaults_auth_to_none() {
        let back: ServerSpec =
            serde_json::from_str(r#"{"transport":"http","url":"https://example.com/mcp"}"#)
                .unwrap();
        assert!(matches!(
            back,
            ServerSpec::Http {
                auth: AuthRef::None,
                ..
            }
        ));
        assert_eq!(back.label(), "https://example.com/mcp");
    }

    #[test]
    fn protocol_modes_parse_their_names_and_default_to_legacy() {
        assert_eq!(ProtocolMode::default(), ProtocolMode::Legacy);
        for mode in ProtocolMode::ALL {
            assert_eq!(ProtocolMode::parse(mode.as_str()), Some(mode));
            assert_eq!(
                serde_json::to_value(mode).unwrap(),
                serde_json::Value::String(mode.as_str().into())
            );
        }
        assert_eq!(ProtocolMode::parse(" Modern "), Some(ProtocolMode::Modern));
        assert_eq!(ProtocolMode::parse("2026-07-28"), None);
    }

    #[test]
    fn a_stdio_label_quotes_an_argument_with_a_space() {
        let stdio = |args: &[&str]| ServerSpec::Stdio {
            command: "srv".into(),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
            env: BTreeMap::new(),
            cwd: None,
        };
        let quoted = stdio(&["a b"]);
        assert_eq!(quoted.label(), "srv 'a b'");
        assert_eq!(
            crate::split_command_line(&quoted.label()).unwrap(),
            ["srv", "a b"]
        );
        assert_eq!(stdio(&["a", "b"]).label(), "srv a b");
        assert_eq!(
            stdio(&["--verbose", "server"]).label(),
            "srv --verbose server"
        );
    }
}
