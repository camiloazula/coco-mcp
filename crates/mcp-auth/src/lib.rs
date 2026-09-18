//! Header auth and OAuth 2.1 (PKCE, dynamic client registration) with
//! keyring storage.
//!
//! The protocol work (discovery from a `WWW-Authenticate` challenge,
//! registration, PKCE, token exchange and refresh) is `rmcp`'s `auth`
//! module. This crate adds what an application needs around it: a secret
//! store backed by the OS keyring, a loopback redirect listener, a browser
//! opener, and [`resolve`] which turns an [`AuthRef`] into the transport
//! options `mcp-core` connects with.
//!
//! UI-free: must never depend on `gpui` or `gpui-kit`.

#![forbid(unsafe_code)]
// unwrap()/expect() are denied in shipped code but fine inside tests.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod credentials;
mod loopback;
mod oauth;
mod secrets;

use std::sync::Arc;

use mcp_core::AuthRef;
use mcp_core::transport::TransportOptions;

pub use credentials::KeyringCredentialStore;
pub use loopback::{Callback, Loopback};
pub use oauth::{OAuthOptions, Opener, authorized_client, browser_opener, forget};
pub use secrets::{KeyringStore, MemoryStore, SecretStore, new_keyring_id};

/// Errors produced by this crate.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The secret store failed.
    #[error("secret store: {0}")]
    Secrets(String),
    /// No secret is stored under the keyring id the spec refers to.
    #[error("no secret stored for `{0}`")]
    MissingSecret(String),
    /// The OAuth flow failed.
    #[error("oauth: {0}")]
    OAuth(#[from] rmcp::transport::auth::AuthError),
    /// The user did not finish the browser flow in time.
    #[error("authorization timed out after {0:?}")]
    Timeout(std::time::Duration),
    /// The loopback listener could not start.
    #[error("loopback listener: {0}")]
    Loopback(#[from] std::io::Error),
    /// The HTTP client could not be built.
    #[error("http client: {0}")]
    Http(#[from] reqwest::Error),
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Turn a server's [`AuthRef`] into transport options: a bearer header read
/// from the secret store, or an OAuth client that has valid credentials
/// (running the browser flow when it has none).
pub async fn resolve(
    auth: &AuthRef,
    url: &str,
    secrets: Arc<dyn SecretStore>,
    options: &OAuthOptions,
) -> Result<TransportOptions> {
    match auth {
        AuthRef::None => Ok(TransportOptions::default()),
        AuthRef::Bearer { keyring_id } => {
            let token = secrets
                .get(keyring_id)
                .map_err(|e| Error::Secrets(e.to_string()))?
                .ok_or_else(|| Error::MissingSecret(keyring_id.clone()))?;
            Ok(TransportOptions {
                bearer_token: Some(token),
                oauth: None,
            })
        }
        AuthRef::OAuth { keyring_id } => {
            let client = authorized_client(url, secrets, keyring_id, options).await?;
            Ok(TransportOptions {
                bearer_token: None,
                oauth: Some(client),
            })
        }
    }
}
