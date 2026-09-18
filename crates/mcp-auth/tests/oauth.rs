//! End-to-end auth against the mock server's HTTP mode: bearer headers,
//! the full OAuth flow with a test opener standing in for the browser,
//! credential reuse, and token refresh.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use mcp_auth::{MemoryStore, OAuthOptions, Opener, SecretStore, resolve};
use mcp_core::{AuthRef, ServerSpec, Session, SessionOptions};
use mcp_mockserver::Schema;
use mcp_mockserver::http::{HttpAuth, serve_http};
use serde_json::json;

fn http_spec(url: &str, auth: AuthRef) -> ServerSpec {
    ServerSpec::Http {
        url: url.into(),
        headers: Default::default(),
        auth,
    }
}

/// Stands in for the browser: follows the authorization redirect, which
/// lands on the loopback listener.
fn test_opener(calls: Arc<AtomicU32>) -> Opener {
    Arc::new(move |url: String| {
        calls.fetch_add(1, Ordering::Relaxed);
        tokio::spawn(async move {
            let client = reqwest::Client::new();
            let response = client.get(url).send().await.expect("authorize");
            assert!(response.status().is_success(), "{}", response.status());
        });
    })
}

#[tokio::test]
async fn plain_http_connects() {
    let server = serve_http(Schema::V1, HttpAuth::None).await.unwrap();
    let session = Session::connect(
        http_spec(&server.url, AuthRef::None),
        SessionOptions::default(),
    )
    .await
    .unwrap();
    let snap = session.snapshot().await.unwrap();
    assert!(snap.tool("echo").is_some());
    let out = session
        .call_tool("echo", json!({"text": "http"}))
        .await
        .unwrap();
    assert_eq!(out.content[0]["text"], "http");
}

#[tokio::test]
async fn bearer_header_is_required_and_read_from_the_store() {
    let server = serve_http(Schema::V1, HttpAuth::Bearer("s3cret".into()))
        .await
        .unwrap();
    let err = Session::connect(
        http_spec(&server.url, AuthRef::None),
        SessionOptions::default(),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, mcp_core::Error::AuthRequired { .. }), "{err}");
    assert!(server.stats.rejected.load(Ordering::Relaxed) >= 1);

    let secrets: Arc<dyn SecretStore> = Arc::new(MemoryStore::new());
    let auth = AuthRef::Bearer {
        keyring_id: "srv-bearer".into(),
    };
    let missing = resolve(
        &auth,
        &server.url,
        secrets.clone(),
        &OAuthOptions::default(),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(missing, mcp_auth::Error::MissingSecret(_)),
        "{missing}"
    );

    secrets.set("srv-bearer", "s3cret").unwrap();
    let transport = resolve(&auth, &server.url, secrets, &OAuthOptions::default())
        .await
        .unwrap();
    assert_eq!(transport.bearer_token.as_deref(), Some("s3cret"));
    let options = SessionOptions {
        transport,
        ..SessionOptions::default()
    };
    let session = Session::connect(http_spec(&server.url, auth), options)
        .await
        .unwrap();
    assert!(session.snapshot().await.unwrap().tool("echo").is_some());
}

#[tokio::test]
async fn oauth_flow_registers_authorizes_reuses_and_refreshes() {
    let server = serve_http(Schema::V1, HttpAuth::OAuth { expires_in: 3600 })
        .await
        .unwrap();
    let secrets: Arc<dyn SecretStore> = Arc::new(MemoryStore::new());
    let opened = Arc::new(AtomicU32::new(0));
    let options = OAuthOptions {
        timeout: Duration::from_secs(10),
        open: test_opener(opened.clone()),
        ..OAuthOptions::default()
    };
    let auth = AuthRef::OAuth {
        keyring_id: "srv-oauth".into(),
    };

    // First run: full flow.
    let transport = resolve(&auth, &server.url, secrets.clone(), &options)
        .await
        .unwrap();
    assert!(transport.oauth.is_some());
    assert_eq!(opened.load(Ordering::Relaxed), 1);
    assert_eq!(server.stats.registrations.load(Ordering::Relaxed), 1);
    assert_eq!(server.stats.authorizations.load(Ordering::Relaxed), 1);
    assert_eq!(server.stats.token_exchanges.load(Ordering::Relaxed), 1);
    let session = Session::connect(
        http_spec(&server.url, auth.clone()),
        SessionOptions {
            transport,
            ..SessionOptions::default()
        },
    )
    .await
    .unwrap();
    let out = session
        .call_tool("echo", json!({"text": "oauth"}))
        .await
        .unwrap();
    assert_eq!(out.content[0]["text"], "oauth");
    assert_eq!(
        secrets
            .get("srv-oauth")
            .unwrap()
            .map(|s| s.contains("access_token")),
        Some(true)
    );

    // Second run: stored credentials, no browser.
    let transport = resolve(&auth, &server.url, secrets.clone(), &options)
        .await
        .unwrap();
    assert!(transport.oauth.is_some());
    assert_eq!(
        opened.load(Ordering::Relaxed),
        1,
        "no second browser round trip"
    );
    assert_eq!(server.stats.token_exchanges.load(Ordering::Relaxed), 1);

    // Forget, then the flow runs again.
    mcp_auth::forget(secrets.as_ref(), "srv-oauth").unwrap();
    let _ = resolve(&auth, &server.url, secrets.clone(), &options)
        .await
        .unwrap();
    assert_eq!(opened.load(Ordering::Relaxed), 2);
    assert_eq!(server.stats.registrations.load(Ordering::Relaxed), 2);
}

#[tokio::test]
async fn short_lived_tokens_are_refreshed_transparently() {
    let server = serve_http(Schema::V1, HttpAuth::OAuth { expires_in: 1 })
        .await
        .unwrap();
    let secrets: Arc<dyn SecretStore> = Arc::new(MemoryStore::new());
    let options = OAuthOptions {
        timeout: Duration::from_secs(10),
        open: test_opener(Arc::new(AtomicU32::new(0))),
        ..OAuthOptions::default()
    };
    let auth = AuthRef::OAuth {
        keyring_id: "srv-short".into(),
    };
    let transport = resolve(&auth, &server.url, secrets, &options)
        .await
        .unwrap();
    let session = Session::connect(
        http_spec(&server.url, auth),
        SessionOptions {
            transport,
            ..SessionOptions::default()
        },
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let out = session
        .call_tool("echo", json!({"text": "still here"}))
        .await
        .unwrap();
    assert_eq!(out.content[0]["text"], "still here");
    assert!(
        server.stats.refreshes.load(Ordering::Relaxed) >= 1,
        "token was refreshed"
    );
}
