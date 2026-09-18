//! The OAuth 2.1 authorization flow for one server, on top of rmcp's
//! `AuthorizationManager`: stored credentials are reused when present;
//! otherwise discovery, registration (or a pre-registered client), PKCE,
//! the browser round trip through the loopback listener, and the token
//! exchange run once, and the result is saved to the secret store.

use std::sync::Arc;
use std::time::Duration;

use rmcp::transport::auth::{
    AuthClient, AuthorizationManager, AuthorizationRequest, AuthorizationSession,
};

use crate::{Error, KeyringCredentialStore, Loopback, Result, SecretStore};

/// Opens the authorization URL for the user (a browser in the app, an HTTP
/// client in tests).
pub type Opener = Arc<dyn Fn(String) + Send + Sync>;

/// Knobs for [`authorized_client`].
#[derive(Clone)]
pub struct OAuthOptions {
    /// `client_name` sent during dynamic registration.
    pub client_name: String,
    /// Requested scopes; empty lets rmcp pick from server metadata.
    pub scopes: Vec<String>,
    /// Pre-registered client id, when the user has one.
    pub client_id: Option<String>,
    /// How long to wait for the browser round trip.
    pub timeout: Duration,
    /// `WWW-Authenticate` challenge from a failed connect, when available.
    pub challenge: Option<String>,
    /// How the authorization URL is presented.
    pub open: Opener,
}

impl std::fmt::Debug for OAuthOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthOptions")
            .field("client_name", &self.client_name)
            .field("scopes", &self.scopes)
            .field("client_id", &self.client_id)
            .field("timeout", &self.timeout)
            .field("challenge", &self.challenge)
            .finish_non_exhaustive()
    }
}

impl Default for OAuthOptions {
    fn default() -> Self {
        Self {
            client_name: "Coco MCP".into(),
            scopes: Vec::new(),
            client_id: None,
            timeout: Duration::from_secs(300),
            challenge: None,
            open: browser_opener(),
        }
    }
}

/// Opener that launches the system browser.
pub fn browser_opener() -> Opener {
    Arc::new(|url: String| {
        if let Err(e) = open::that_detached(&url) {
            tracing::error!("cannot open browser for {url}: {e}");
        }
    })
}

/// An HTTP client that attaches (and refreshes) the server's token, running
/// the browser flow first when the secret store holds no credentials.
pub async fn authorized_client(
    url: &str,
    secrets: Arc<dyn SecretStore>,
    keyring_id: &str,
    options: &OAuthOptions,
) -> Result<AuthClient<reqwest::Client>> {
    let http = reqwest::Client::builder().build()?;
    let mut manager = AuthorizationManager::new(url).await?;
    manager.set_credential_store(KeyringCredentialStore::new(secrets, keyring_id));
    if manager.initialize_from_store().await? {
        tracing::debug!("reusing stored credentials for {url}");
        return Ok(AuthClient::new(http, manager));
    }

    let resolution = manager
        .resolve_metadata_from_challenge(options.challenge.as_deref())
        .await?;
    manager.set_metadata(resolution.metadata);

    let listener = Loopback::bind().await?;
    let mut request = AuthorizationRequest::new(listener.redirect_uri.clone())
        .with_client_name(options.client_name.clone())
        .with_scopes(options.scopes.clone());
    if let Some(client_id) = &options.client_id {
        request = request.with_preregistered_client(client_id.clone());
    }
    let session = AuthorizationSession::new(manager, request)
        .await
        .map_err(|(_, e)| Error::OAuth(e))?;
    let auth_url = browser_url(&session.auth_url)?;
    if let Some((_, state)) = auth_url.query_pairs().find(|(key, _)| key == "state") {
        listener.expect_state(state.into_owned());
    }
    (options.open)(session.auth_url.clone());
    let callback = listener.wait(options.timeout).await?;
    session
        .handle_callback_with_issuer(&callback.code, &callback.state, callback.issuer.as_deref())
        .await?;
    Ok(AuthClient::new(http, session.auth_manager))
}

/// The authorization endpoint as a URL a browser may be sent to: `https`,
/// or `http` on the local machine. The endpoint comes from metadata the
/// server pointed at, so a `file:` or custom scheme here would launch
/// whatever handles it, not a sign-in page.
fn browser_url(url: &str) -> Result<reqwest::Url> {
    let failed = |message: String| {
        Error::OAuth(rmcp::transport::auth::AuthError::AuthorizationFailed(
            message,
        ))
    };
    let parsed = reqwest::Url::parse(url)
        .map_err(|e| failed(format!("authorization endpoint is not a URL: {e}")))?;
    let local = matches!(
        parsed.host_str(),
        Some("127.0.0.1" | "localhost" | "[::1]" | "::1")
    );
    match parsed.scheme() {
        "https" => Ok(parsed),
        "http" if local => Ok(parsed),
        scheme => Err(failed(format!(
            "authorization endpoint must be https, not `{scheme}:` ({url})"
        ))),
    }
}

/// Remove stored credentials for a server.
pub fn forget(secrets: &dyn SecretStore, keyring_id: &str) -> Result<()> {
    secrets.delete(keyring_id).map_err(Error::Secrets)
}
