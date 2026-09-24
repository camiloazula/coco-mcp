//! Streamable-HTTP mode with optional bearer auth or a mock OAuth 2.1
//! authorization server (RFC 9728 resource metadata, RFC 8414 server
//! metadata, RFC 7591 dynamic registration, PKCE S256, refresh tokens).
//! Everything auto-approves so tests can drive the flow without a browser.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Form, Query, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine as _;
use rmcp::transport::StreamableHttpServerConfig;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::tower::StreamableHttpService;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use crate::{Faults, MockServer, Schema};

/// How the HTTP endpoint is protected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpAuth {
    /// Open.
    None,
    /// Requires `Authorization: Bearer <token>` with this exact token.
    Bearer(String),
    /// Requires a token issued by the built-in authorization server.
    OAuth {
        /// `expires_in` reported for issued access tokens, in seconds.
        expires_in: u64,
    },
}

/// Counters the tests assert on.
#[derive(Debug, Default)]
pub struct OAuthStats {
    /// `POST /register` calls.
    pub registrations: AtomicU32,
    /// `GET /authorize` calls.
    pub authorizations: AtomicU32,
    /// `authorization_code` grants.
    pub token_exchanges: AtomicU32,
    /// `refresh_token` grants.
    pub refreshes: AtomicU32,
    /// Requests to `/mcp` rejected with 401.
    pub rejected: AtomicU32,
}

/// A running HTTP mock.
#[derive(Debug)]
pub struct HttpServer {
    /// `http://127.0.0.1:<port>`.
    pub base: String,
    /// The MCP endpoint (`<base>/mcp`).
    pub url: String,
    /// Auth counters.
    pub stats: Arc<OAuthStats>,
    state: Arc<Mutex<AuthState>>,
    shutdown: CancellationToken,
}

impl HttpServer {
    /// Stop serving.
    pub fn shutdown(&self) {
        self.shutdown.cancel();
    }

    /// Refuse every token from now on, bearer or issued, as a server does
    /// once a token has expired: a client connected before sees `401` on
    /// its next request.
    pub fn revoke_tokens(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.revoked = true;
        }
    }
}

impl Drop for HttpServer {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

#[derive(Debug)]
struct Issued {
    expires_at: Instant,
}

#[derive(Debug, Default)]
struct AuthState {
    codes: HashMap<String, String>, // code -> pkce challenge
    access: HashMap<String, Issued>,
    refresh: HashMap<String, ()>,
    counter: u32,
    /// Every token is refused from now on, as after an expiry.
    revoked: bool,
}

#[derive(Clone)]
struct Shared {
    base: String,
    auth: HttpAuth,
    stats: Arc<OAuthStats>,
    state: Arc<Mutex<AuthState>>,
}

impl Shared {
    fn token_ok(&self, header: Option<&HeaderValue>) -> bool {
        if self.state.lock().is_ok_and(|s| s.revoked) {
            return false;
        }
        let token = header
            .and_then(|h| h.to_str().ok())
            .and_then(|h| h.strip_prefix("Bearer "))
            .map(str::trim);
        match (&self.auth, token) {
            (HttpAuth::None, _) => true,
            (HttpAuth::Bearer(expected), Some(t)) => t == expected,
            (HttpAuth::OAuth { .. }, Some(t)) => self
                .state
                .lock()
                .ok()
                .and_then(|s| s.access.get(t).map(|i| i.expires_at > Instant::now()))
                .unwrap_or(false),
            (_, None) => false,
        }
    }

    fn next_id(&self, prefix: &str) -> String {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.counter += 1;
        format!("{prefix}-{}", s.counter)
    }

    fn issue_tokens(&self) -> Value {
        let expires_in = match self.auth {
            HttpAuth::OAuth { expires_in } => expires_in,
            _ => 3600,
        };
        let access = self.next_id("at");
        let refresh = self.next_id("rt");
        {
            let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            s.access.insert(
                access.clone(),
                Issued {
                    expires_at: Instant::now() + Duration::from_secs(expires_in.max(1)),
                },
            );
            s.refresh.insert(refresh.clone(), ());
        }
        json!({
            "access_token": access,
            "token_type": "Bearer",
            "expires_in": expires_in,
            "refresh_token": refresh,
            "scope": "mcp"
        })
    }
}

async fn require_auth(State(shared): State<Shared>, req: Request, next: Next) -> Response {
    if shared.token_ok(req.headers().get(header::AUTHORIZATION)) {
        return next.run(req).await;
    }
    shared.stats.rejected.fetch_add(1, Ordering::Relaxed);
    let challenge = match shared.auth {
        HttpAuth::OAuth { .. } => format!(
            "Bearer resource_metadata=\"{}/.well-known/oauth-protected-resource\"",
            shared.base
        ),
        _ => "Bearer".to_string(),
    };
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, challenge)],
        "unauthorized",
    )
        .into_response()
}

async fn protected_resource(State(shared): State<Shared>) -> Json<Value> {
    Json(json!({
        "resource": format!("{}/mcp", shared.base),
        "authorization_servers": [shared.base],
        "scopes_supported": ["mcp"],
        "bearer_methods_supported": ["header"]
    }))
}

async fn server_metadata(State(shared): State<Shared>) -> Json<Value> {
    Json(json!({
        "issuer": shared.base,
        "authorization_endpoint": format!("{}/authorize", shared.base),
        "token_endpoint": format!("{}/token", shared.base),
        "registration_endpoint": format!("{}/register", shared.base),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        "scopes_supported": ["mcp"]
    }))
}

async fn register(
    State(shared): State<Shared>,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    shared.stats.registrations.fetch_add(1, Ordering::Relaxed);
    let client_id = shared.next_id("client");
    (
        StatusCode::CREATED,
        Json(json!({
            "client_id": client_id,
            "client_name": body.get("client_name").cloned().unwrap_or(Value::Null),
            "redirect_uris": body.get("redirect_uris").cloned().unwrap_or(json!([])),
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none"
        })),
    )
}

async fn authorize(
    State(shared): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    shared.stats.authorizations.fetch_add(1, Ordering::Relaxed);
    let (Some(redirect), Some(state), Some(challenge)) = (
        q.get("redirect_uri"),
        q.get("state"),
        q.get("code_challenge"),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "missing redirect_uri, state or code_challenge",
        )
            .into_response();
    };
    if q.get("code_challenge_method").map(String::as_str) != Some("S256") {
        return (StatusCode::BAD_REQUEST, "only S256 is supported").into_response();
    }
    let code = shared.next_id("code");
    if let Ok(mut s) = shared.state.lock() {
        s.codes.insert(code.clone(), challenge.clone());
    }
    let sep = if redirect.contains('?') { '&' } else { '?' };
    Redirect::to(&format!(
        "{redirect}{sep}code={code}&state={state}&iss={}",
        shared.base
    ))
    .into_response()
}

async fn token(
    State(shared): State<Shared>,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    let bad = |msg: &str| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid_grant", "error_description": msg})),
        )
            .into_response()
    };
    match form.get("grant_type").map(String::as_str) {
        Some("authorization_code") => {
            let (Some(code), Some(verifier)) = (form.get("code"), form.get("code_verifier")) else {
                return bad("missing code or code_verifier");
            };
            let challenge = shared
                .state
                .lock()
                .ok()
                .and_then(|mut s| s.codes.remove(code));
            let Some(challenge) = challenge else {
                return bad("unknown code");
            };
            let digest = Sha256::digest(verifier.as_bytes());
            let expected = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
            if expected != challenge {
                return bad("pkce verification failed");
            }
            shared.stats.token_exchanges.fetch_add(1, Ordering::Relaxed);
            Json(shared.issue_tokens()).into_response()
        }
        Some("refresh_token") => {
            let Some(refresh) = form.get("refresh_token") else {
                return bad("missing refresh_token");
            };
            let known = shared
                .state
                .lock()
                .ok()
                .map(|mut s| s.refresh.remove(refresh).is_some())
                .unwrap_or(false);
            if !known {
                return bad("unknown refresh_token");
            }
            shared.stats.refreshes.fetch_add(1, Ordering::Relaxed);
            Json(shared.issue_tokens()).into_response()
        }
        _ => bad("unsupported grant_type"),
    }
}

/// Serve `schema` over streamable HTTP on a random loopback port.
pub async fn serve_http(schema: Schema, auth: HttpAuth) -> anyhow::Result<HttpServer> {
    serve_http_at(schema, auth, "127.0.0.1:0").await
}

/// Serve `schema` over streamable HTTP on `addr`.
pub async fn serve_http_at(
    schema: Schema,
    auth: HttpAuth,
    addr: &str,
) -> anyhow::Result<HttpServer> {
    serve_http_with(schema, Faults::default(), auth, addr).await
}

/// Serve `schema`, misbehaving as `faults` says, over streamable HTTP on
/// `addr`.
pub async fn serve_http_with(
    schema: Schema,
    faults: Faults,
    auth: HttpAuth,
    addr: &str,
) -> anyhow::Result<HttpServer> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let addr = listener.local_addr()?;
    let base = format!("http://{addr}");
    let shutdown = CancellationToken::new();
    let shared = Shared {
        base: base.clone(),
        auth,
        stats: Arc::new(OAuthStats::default()),
        state: Arc::new(Mutex::new(AuthState::default())),
    };
    // One server behind every session: a 2026-07-28 request is served without
    // a session and must see what the requests before it did, and a change
    // one client makes reaches the subscription streams of the others.
    let server = MockServer::with_faults(schema, faults);
    let service: StreamableHttpService<MockServer, LocalSessionManager> =
        StreamableHttpService::new(
            move || Ok(server.clone()),
            Default::default(),
            StreamableHttpServerConfig::default()
                .with_sse_keep_alive(None)
                .with_cancellation_token(shutdown.child_token()),
        );
    let mcp =
        Router::new()
            .nest_service("/mcp", service)
            .layer(axum::middleware::from_fn_with_state(
                shared.clone(),
                require_auth,
            ));
    let router = Router::new()
        .route(
            "/.well-known/oauth-protected-resource",
            get(protected_resource),
        )
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(protected_resource),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            get(server_metadata),
        )
        .route("/register", post(register))
        .route("/authorize", get(authorize))
        .route("/token", post(token))
        .merge(mcp)
        .with_state(shared.clone());
    let ct = shutdown.clone();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(async move { ct.cancelled_owned().await })
            .await;
    });
    Ok(HttpServer {
        url: format!("{base}/mcp"),
        base,
        stats: shared.stats.clone(),
        state: shared.state.clone(),
        shutdown,
    })
}
