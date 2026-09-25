//! Error type for `mcp-core`.

use std::time::Duration;

/// Errors produced by the core client.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The server spec could not be turned into a transport.
    #[error("invalid server spec: {0}")]
    InvalidSpec(String),
    /// Transport-level failure (spawn, connect, I/O).
    #[error("transport error: {0}")]
    Transport(String),
    /// The session did not start: the handshake or `server/discover` failed.
    #[error("connect failed: {0}")]
    Initialize(String),
    /// The server returned a JSON-RPC error.
    #[error("server error {code}: {message}")]
    Server {
        /// JSON-RPC error code.
        code: i64,
        /// Human-readable message from the server.
        message: String,
        /// Optional structured data.
        data: Option<serde_json::Value>,
    },
    /// The request did not complete in time.
    #[error("request timed out after {0:?}")]
    Timeout(Duration),
    /// The connection is closed.
    #[error("connection closed")]
    Closed,
    /// The caller cancelled the request; the server was told with
    /// `notifications/cancelled`.
    #[error("cancelled")]
    Cancelled,
    /// The server answered `401`/`403`; `challenge` is its `WWW-Authenticate`
    /// header when present. Configure auth (bearer or OAuth) and reconnect.
    #[error("authorization required{}", challenge.as_deref().map(|c| format!(": {c}")).unwrap_or_default())]
    AuthRequired {
        /// The `WWW-Authenticate` challenge, if any.
        challenge: Option<String>,
    },
    /// Arguments were not acceptable before sending.
    #[error("invalid arguments: {0}")]
    InvalidArguments(String),
    /// Something asked of the client was turned down: an input request the
    /// policy refuses, or a subscription the server did not accept.
    #[error("refused: {0}")]
    Refused(String),
    /// The server kept answering `input_required` past the round limit.
    #[error("the server still asked for input after {0} rounds")]
    InputRounds(usize),
    /// JSON (de)serialization failure.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl Error {
    /// Whether the server said the resource does not exist: `-32602` with
    /// the URI in its data from 2026-07-28 on, `-32002` from older servers.
    pub fn is_resource_not_found(&self) -> bool {
        match self {
            Self::Server { code: -32002, .. } => true,
            Self::Server {
                code: -32602, data, ..
            } => data.as_ref().and_then(|d| d.get("uri")).is_some(),
            _ => false,
        }
    }
}

impl From<rmcp::ServiceError> for Error {
    fn from(e: rmcp::ServiceError) -> Self {
        match e {
            rmcp::ServiceError::McpError(data) => Self::Server {
                code: i64::from(data.code.0),
                message: data.message.into_owned(),
                data: data.data,
            },
            rmcp::ServiceError::TransportClosed => Self::Closed,
            rmcp::ServiceError::Timeout { timeout } => Self::Timeout(timeout),
            // A `401`/`403` to a request of a running session: the token was
            // refused or has expired since the handshake.
            rmcp::ServiceError::TransportSend(e) => match auth_challenge(&e) {
                Some(challenge) => Self::AuthRequired { challenge },
                None => Self::Transport(transport_cause(&e)),
            },
            other => Self::Transport(other.to_string()),
        }
    }
}

/// The `WWW-Authenticate` challenge behind a transport error the server
/// answered with `401`/`403`, wherever it sits in the error's chain of
/// causes; `None` for any other failure.
fn auth_challenge(error: &(dyn std::error::Error + 'static)) -> Option<Option<String>> {
    use rmcp::transport::streamable_http_client::AuthRequiredError;
    let mut current = Some(error);
    while let Some(e) = current {
        if let Some(auth) = e.downcast_ref::<AuthRequiredError>() {
            let header = auth.www_authenticate_header.trim();
            return Some((!header.is_empty()).then(|| header.to_owned()));
        }
        current = e.source();
    }
    None
}

/// What went wrong under a transport's failure, in its own words: the
/// innermost cause, such as `connection refused`, where the layers above it
/// only say where it happened (`error sending request for url (…)`). The
/// HTTP transport does not chain its client's error as a cause, so the walk
/// steps into it by hand.
pub(crate) fn transport_cause(error: &(dyn std::error::Error + 'static)) -> String {
    use rmcp::transport::streamable_http_client::StreamableHttpError;
    let mut deepest = error;
    loop {
        let next = deepest.source().or_else(|| {
            match deepest.downcast_ref::<StreamableHttpError<reqwest::Error>>()? {
                StreamableHttpError::Client(client) => Some(client as _),
                _ => None,
            }
        });
        match next {
            Some(e) => deepest = e,
            None => break,
        }
    }
    plain(&deepest.to_string())
}

/// An operating system's error text as a clause: `Connection refused (os
/// error 61)` reads as `connection refused`. An acronym keeps its case.
fn plain(text: &str) -> String {
    let text = match text.rfind(" (os error ") {
        Some(at) if text.ends_with(')') => &text[..at],
        _ => text,
    };
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(first), Some(second)) if first.is_uppercase() && second.is_lowercase() => first
            .to_lowercase()
            .chain(text[first.len_utf8()..].chars())
            .collect(),
        _ => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::transport::streamable_http_client::AuthRequiredError;

    /// A transport's failure with the refusal further down its chain of
    /// causes, as `rmcp` wraps it.
    #[derive(Debug)]
    struct Wrapped(AuthRequiredError);

    impl std::fmt::Display for Wrapped {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "send failed: {}", self.0)
        }
    }

    impl std::error::Error for Wrapped {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&self.0)
        }
    }

    #[test]
    fn a_refusal_is_found_down_the_chain() {
        let refused = Wrapped(AuthRequiredError::new("Bearer".into()));
        assert_eq!(auth_challenge(&refused), Some(Some("Bearer".to_owned())));
        let bare = Wrapped(AuthRequiredError::new("  ".into()));
        assert_eq!(
            auth_challenge(&bare),
            Some(None),
            "an empty header is no challenge"
        );
        let other = std::io::Error::other("connection refused");
        assert_eq!(auth_challenge(&other), None);
    }

    #[test]
    fn a_transport_failure_is_told_by_its_innermost_cause() {
        let refused = std::io::Error::from(std::io::ErrorKind::ConnectionRefused);
        let wrapped = Sending(Box::new(refused));
        assert_eq!(transport_cause(&wrapped), "connection refused");
        assert_eq!(
            plain("Connection refused (os error 61)"),
            "connection refused"
        );
        assert_eq!(plain("HTTP 404"), "HTTP 404", "an acronym keeps its case");
        assert_eq!(
            plain("unexpected end of stream"),
            "unexpected end of stream"
        );
    }

    #[tokio::test]
    async fn an_http_client_error_is_stepped_into() {
        use rmcp::transport::streamable_http_client::StreamableHttpError;
        // Port 9 (discard) has nothing listening: the connect is refused.
        let Err(client) = reqwest::Client::new()
            .post("http://127.0.0.1:9/mcp")
            .send()
            .await
        else {
            panic!("nothing listens on port 9");
        };
        let top = client.to_string();
        let error: StreamableHttpError<reqwest::Error> = StreamableHttpError::Client(client);
        let cause = transport_cause(&error);
        assert!(
            !cause.contains("error sending request"),
            "{cause} from {top}"
        );
        assert!(cause.contains("refused"), "{cause}");
    }

    /// A failure whose cause is chained the ordinary way.
    #[derive(Debug)]
    struct Sending(Box<dyn std::error::Error + Send + Sync>);

    impl std::fmt::Display for Sending {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "error sending request: {}", self.0)
        }
    }

    impl std::error::Error for Sending {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&*self.0)
        }
    }

    #[test]
    fn the_other_service_errors_keep_their_kinds() {
        assert!(matches!(
            Error::from(rmcp::ServiceError::TransportClosed),
            Error::Closed
        ));
        assert!(matches!(
            Error::from(rmcp::ServiceError::Timeout {
                timeout: std::time::Duration::from_secs(1)
            }),
            Error::Timeout(_)
        ));
    }
}
