//! What the window says when something fails: one sentence a person can
//! act on, with the detail that matters kept after it. The log drawer shows
//! every error as it came and never goes through here.

use mcp_core::{AuthRef, Error, ServerSpec};

/// `error` as a sentence for the pane it is shown in. `spec` is the server
/// the failure came from, when known: it names the address that could not
/// be reached and the kind of credential the server refused.
pub fn explain(error: &Error, spec: Option<&ServerSpec>) -> String {
    match error {
        Error::InvalidSpec(what) => sentence(format!("The server settings are not valid: {what}")),
        Error::Transport(detail) => {
            let detail = tidy(detail);
            sentence(match spec {
                Some(ServerSpec::Http { url, .. }) => {
                    format!("Could not reach the server at {url}: {detail}")
                }
                Some(ServerSpec::Stdio { command, .. }) => {
                    format!("The server process `{command}` did not answer: {detail}")
                }
                None => format!("Could not reach the server: {detail}"),
            })
        }
        Error::Initialize(detail) => sentence(format!(
            "The server did not complete the handshake: {}",
            tidy(detail)
        )),
        Error::Server { code, message, .. } => {
            sentence(format!("{} (server error {code})", message.trim()))
        }
        Error::Timeout(after) => sentence(format!("The server did not answer within {after:?}")),
        Error::Closed => "The connection was closed.".to_owned(),
        Error::Cancelled => "The request was cancelled.".to_owned(),
        Error::AuthRequired { .. } => match spec {
            Some(ServerSpec::Http {
                auth: AuthRef::Bearer { .. },
                ..
            }) => "The server refused the token. Update it in the server settings.",
            Some(ServerSpec::Http {
                auth: AuthRef::OAuth { .. },
                ..
            }) => "The server needs authorization. Authorize to sign in again.",
            _ => {
                "The server needs credentials. Add a token or authorization in the server settings."
            }
        }
        .to_owned(),
        Error::InvalidArguments(what) => {
            sentence(format!("The arguments were not accepted: {what}"))
        }
        Error::Refused(what) => sentence(format!("Turned down: {what}")),
        Error::InputRounds(rounds) => {
            format!("The server still asked for input after {rounds} rounds.")
        }
        Error::Json(e) => sentence(format!("The answer could not be read: {e}")),
    }
}

/// `text` ending in one full stop.
fn sentence(text: String) -> String {
    format!("{}.", text.trim().trim_end_matches('.'))
}

/// A transport's error text without the Rust type paths in brackets and
/// the framing around them: `Send message error Transport [a::B<c::D>]
/// error: Auth required, when send initialize request` reads as `Auth
/// required, when send initialize request`.
fn tidy(detail: &str) -> String {
    let mut out = String::with_capacity(detail.len());
    let mut depth = 0usize;
    for c in detail.chars() {
        match c {
            '[' => depth += 1,
            ']' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    let words: Vec<&str> = out.split_whitespace().collect();
    let text = words.join(" ");
    let mut text = text.as_str();
    for noise in [
        "Send message error Transport error: ",
        "Transport error: ",
        "Client error: ",
    ] {
        if let Some(rest) = text.strip_prefix(noise) {
            text = rest;
        }
    }
    text.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn http(auth: AuthRef) -> ServerSpec {
        ServerSpec::Http {
            url: "http://127.0.0.1:9/mcp".into(),
            headers: Default::default(),
            auth,
        }
    }

    #[test]
    fn a_refused_token_names_the_setting() {
        let refused = Error::AuthRequired {
            challenge: Some("Bearer".into()),
        };
        let bearer = http(AuthRef::Bearer {
            keyring_id: "k".into(),
        });
        assert_eq!(
            explain(&refused, Some(&bearer)),
            "The server refused the token. Update it in the server settings."
        );
        let oauth = http(AuthRef::OAuth {
            keyring_id: "k".into(),
        });
        assert!(explain(&refused, Some(&oauth)).starts_with("The server needs authorization"));
        assert!(explain(&refused, None).starts_with("The server needs credentials"));
    }

    #[test]
    fn a_transport_failure_names_the_address_without_type_paths() {
        let failed = Error::Transport(
            "Send message error Transport [mcp_core::transport::TracedTransport<mcp_core::transport::HttpTransport>] error: connection refused, when send initialize request".into(),
        );
        assert_eq!(
            explain(&failed, Some(&http(AuthRef::None))),
            "Could not reach the server at http://127.0.0.1:9/mcp: connection refused, when send initialize request."
        );
        let spawn = Error::Transport("failed to spawn `nope`: No such file or directory.".into());
        let stdio = ServerSpec::Stdio {
            command: "nope".into(),
            args: Vec::new(),
            env: Default::default(),
            cwd: None,
        };
        assert_eq!(
            explain(&spawn, Some(&stdio)),
            "The server process `nope` did not answer: failed to spawn `nope`: No such file or directory."
        );
    }

    #[test]
    fn the_rest_read_as_sentences() {
        let server = Error::Server {
            code: -32601,
            message: "Method not found".into(),
            data: None,
        };
        assert_eq!(
            explain(&server, None),
            "Method not found (server error -32601)."
        );
        assert_eq!(
            explain(&Error::Timeout(std::time::Duration::from_secs(30)), None),
            "The server did not answer within 30s."
        );
        assert_eq!(explain(&Error::Closed, None), "The connection was closed.");
        assert_eq!(
            explain(
                &Error::Initialize("the server supports protocol 1, not 2".into()),
                None
            ),
            "The server did not complete the handshake: the server supports protocol 1, not 2."
        );
    }
}
