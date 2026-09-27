//! The Kite Connect login flow (`docs/integrations/kite.md`, "Login and session").
//!
//! 1. The owner opens [`login_url`]; `state` travels back in `redirect_params`.
//! 2. Zerodha redirects to the registered URL with `request_token` and `state`.
//! 3. [`create_session`] exchanges the token, signed with the API secret.
//!
//! The caller checks `state` (against login CSRF) and that the session's
//! `user_id` is the configured one before storing the access token.

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::client::{KiteClient, KiteError};

/// Zerodha's login page.
pub const LOGIN_BASE: &str = "https://kite.zerodha.com/connect/login";

/// A Kite session.
#[derive(Clone, PartialEq, Eq)]
pub struct KiteSession {
    /// The Zerodha client id the session belongs to.
    pub user_id: String,
    /// The access token (secret; valid until 06:00 IST the next day).
    pub access_token: String,
}

impl std::fmt::Debug for KiteSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KiteSession")
            .field("user_id", &self.user_id)
            .field("access_token", &"***")
            .finish()
    }
}

/// The URL that starts a login; `state` comes back with the redirect.
#[must_use]
pub fn login_url(api_key: &str, state: &str) -> String {
    let redirect_params: String = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("state", state)
        .finish();
    let query: String = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("v", "3")
        .append_pair("api_key", api_key)
        .append_pair("redirect_params", &redirect_params)
        .finish();
    format!("{LOGIN_BASE}?{query}")
}

/// `sha256(api_key + request_token + api_secret)`, hex.
#[must_use]
pub fn checksum(api_key: &str, request_token: &str, api_secret: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(api_key.as_bytes());
    hasher.update(request_token.as_bytes());
    hasher.update(api_secret.as_bytes());
    hex::encode(hasher.finalize())
}

/// Exchanges a request token for a session (`POST /session/token`).
pub async fn create_session(
    client: &KiteClient,
    request_token: &str,
    api_secret: &str,
) -> Result<KiteSession, KiteError> {
    let data = client
        .post_form(
            "/session/token",
            &[
                ("api_key", client.api_key().to_owned()),
                ("request_token", request_token.to_owned()),
                (
                    "checksum",
                    checksum(client.api_key(), request_token, api_secret),
                ),
            ],
            false,
        )
        .await?;
    let field = |name: &str| {
        data.get(name)
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| KiteError::Unexpected(format!("session without {name}")))
    };
    Ok(KiteSession {
        user_id: field("user_id")?,
        access_token: field("access_token")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_checksum_is_sha256_of_key_token_and_secret() {
        // sha256("abc") is the standard test vector.
        assert_eq!(
            checksum("a", "b", "c"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn the_login_url_carries_the_state_in_redirect_params() {
        let url = login_url("key1", "s/t=1");
        assert_eq!(
            url,
            "https://kite.zerodha.com/connect/login?v=3&api_key=key1&redirect_params=state%3Ds%252Ft%253D1"
        );
    }
}
