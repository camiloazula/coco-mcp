//! Loopback redirect listener on `127.0.0.1:<random>` for the OAuth
//! authorization code. Serves one callback, then shuts down.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::Html;
use axum::routing::get;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

/// The query parameters of the redirect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Callback {
    /// Authorization code.
    pub code: String,
    /// `state` (rmcp's CSRF token).
    pub state: String,
    /// RFC 9207 `iss`, when the server sends it.
    pub issuer: Option<String>,
}

type Slot = Arc<Mutex<Option<oneshot::Sender<Result<Callback, String>>>>>;

/// What the handler shares with the listener: the one-shot answer, and the
/// `state` the redirect must carry once it is known.
#[derive(Clone)]
struct Shared {
    slot: Slot,
    expected: Arc<Mutex<Option<String>>>,
}

/// A bound listener waiting for the redirect.
#[derive(Debug)]
pub struct Loopback {
    /// `http://127.0.0.1:<port>/callback`.
    pub redirect_uri: String,
    rx: Option<oneshot::Receiver<Result<Callback, String>>>,
    expected: Arc<Mutex<Option<String>>>,
    shutdown: CancellationToken,
}

const PAGE_OK: &str = "<!doctype html><meta charset=utf-8><title>Coco MCP</title>\
<body style=\"font-family:system-ui;background:#161719;color:#e3e4e6;display:grid;place-items:center;height:100vh;margin:0\">\
<div style=\"text-align:center\"><p style=\"font-size:15px\">Signed in.</p><p style=\"color:#8a8d93;font-size:13px\">You can close this window and return to Coco MCP.</p></div>";
const PAGE_ERR: &str = "<!doctype html><meta charset=utf-8><title>Coco MCP</title>\
<body style=\"font-family:system-ui;background:#161719;color:#e3e4e6;display:grid;place-items:center;height:100vh;margin:0\">\
<div style=\"text-align:center\"><p style=\"font-size:15px\">Authorization failed.</p><p style=\"color:#8a8d93;font-size:13px\">Return to Coco MCP for details.</p></div>";

async fn callback(
    State(shared): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
) -> (StatusCode, Html<&'static str>) {
    // The port is guessable and any local page can send a GET here: a
    // request without the state this flow issued is turned away without
    // consuming the one answer, so the real redirect still finds a listener.
    let expected = shared.expected.lock().ok().and_then(|e| e.clone());
    if let Some(expected) = expected
        && q.get("state") != Some(&expected)
    {
        return (StatusCode::BAD_REQUEST, Html(PAGE_ERR));
    }
    let slot = shared.slot;
    let result = match (q.get("code"), q.get("state")) {
        (Some(code), Some(state)) => Ok(Callback {
            code: code.clone(),
            state: state.clone(),
            issuer: q.get("iss").cloned(),
        }),
        _ => Err(q
            .get("error_description")
            .or_else(|| q.get("error"))
            .cloned()
            .unwrap_or_else(|| "redirect without code and state".into())),
    };
    let ok = result.is_ok();
    if let Ok(mut guard) = slot.lock()
        && let Some(tx) = guard.take()
    {
        let _ = tx.send(result);
    }
    if ok {
        (StatusCode::OK, Html(PAGE_OK))
    } else {
        (StatusCode::BAD_REQUEST, Html(PAGE_ERR))
    }
}

impl Loopback {
    /// Bind a random loopback port and start serving `/callback`.
    pub async fn bind() -> std::io::Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let (tx, rx) = oneshot::channel();
        let slot: Slot = Arc::new(Mutex::new(Some(tx)));
        let expected = Arc::new(Mutex::new(None));
        let shutdown = CancellationToken::new();
        let router = Router::new()
            .route("/callback", get(callback))
            .with_state(Shared {
                slot,
                expected: expected.clone(),
            });
        let ct = shutdown.clone();
        tokio::spawn(async move {
            let _ = axum::serve(listener, router)
                .with_graceful_shutdown(async move { ct.cancelled_owned().await })
                .await;
        });
        Ok(Self {
            redirect_uri: format!("http://127.0.0.1:{port}/callback"),
            rx: Some(rx),
            expected,
            shutdown,
        })
    }

    /// The `state` the redirect must carry. Known only once the
    /// authorization URL is built, which needs this listener's address
    /// first; until it is set, any redirect is taken.
    pub fn expect_state(&self, state: String) {
        if let Ok(mut expected) = self.expected.lock() {
            *expected = Some(state);
        }
    }

    /// Wait for the redirect, at most `timeout`.
    pub async fn wait(mut self, timeout: std::time::Duration) -> crate::Result<Callback> {
        let Some(rx) = self.rx.take() else {
            return Err(crate::Error::OAuth(
                rmcp::transport::auth::AuthError::InternalError("listener already consumed".into()),
            ));
        };
        let result = tokio::time::timeout(timeout, rx).await;
        self.shutdown.cancel();
        match result {
            Ok(Ok(Ok(callback))) => Ok(callback),
            Ok(Ok(Err(message))) => Err(crate::Error::OAuth(
                rmcp::transport::auth::AuthError::AuthorizationFailed(message),
            )),
            Ok(Err(_)) => Err(crate::Error::OAuth(
                rmcp::transport::auth::AuthError::AuthorizationFailed(
                    "listener closed before the redirect".into(),
                ),
            )),
            Err(_) => Err(crate::Error::Timeout(timeout)),
        }
    }
}

impl Drop for Loopback {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn receives_code_and_state() {
        let listener = Loopback::bind().await.unwrap();
        let url = format!("{}?code=abc&state=xyz&iss=http://as", listener.redirect_uri);
        tokio::spawn(async move {
            let body = reqwest::get(url).await.unwrap().text().await.unwrap();
            assert!(body.contains("Signed in"));
        });
        let cb = listener
            .wait(std::time::Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(cb.code, "abc");
        assert_eq!(cb.state, "xyz");
        assert_eq!(cb.issuer.as_deref(), Some("http://as"));
    }

    #[tokio::test]
    async fn a_redirect_with_another_state_is_turned_away() {
        let listener = Loopback::bind().await.unwrap();
        listener.expect_state("xyz".into());
        let forged = format!("{}?code=evil&state=nope", listener.redirect_uri);
        let real = format!("{}?code=abc&state=xyz", listener.redirect_uri);
        tokio::spawn(async move {
            let response = reqwest::get(forged).await.unwrap();
            assert_eq!(response.status(), 400);
            let response = reqwest::get(real).await.unwrap();
            assert_eq!(response.status(), 200);
        });
        let cb = listener
            .wait(std::time::Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(cb.code, "abc");
    }

    #[tokio::test]
    async fn reports_errors_and_timeouts() {
        let listener = Loopback::bind().await.unwrap();
        let url = format!("{}?error=access_denied", listener.redirect_uri);
        tokio::spawn(async move {
            let _ = reqwest::get(url).await;
        });
        let err = listener
            .wait(std::time::Duration::from_secs(5))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("access_denied"), "{err}");

        let listener = Loopback::bind().await.unwrap();
        let err = listener
            .wait(std::time::Duration::from_millis(50))
            .await
            .unwrap_err();
        assert!(matches!(err, crate::Error::Timeout(_)));
    }
}
