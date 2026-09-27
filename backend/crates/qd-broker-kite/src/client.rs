//! The Kite Connect v3 HTTP client (`docs/integrations/kite.md`).
//!
//! Every request carries `X-Kite-Version: 3`; authenticated requests carry
//! `Authorization: token api_key:access_token`. Responses are the envelope
//! `{"status": "success", "data": ...}` or `{"status": "error", "message":
//! ..., "error_type": ...}`. The access token is never logged.

use std::time::Duration;

use serde_json::Value;
use thiserror::Error;

/// The production API.
pub const API_BASE: &str = "https://api.kite.trade";

/// Why a Kite call failed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum KiteError {
    /// The session expired or is invalid: log in again.
    #[error("Kite session expired or invalid; log in with Zerodha again: {0}")]
    Token(String),
    /// Kite refused the request (bad input, margin, holdings, order or user
    /// errors). Nothing was done.
    #[error("Kite refused the request ({kind}): {message}")]
    Refused {
        /// Kite's exception name.
        kind: String,
        /// Kite's message.
        message: String,
    },
    /// Transport failure, timeout or a Kite-side failure: the outcome is unknown.
    #[error("Kite unreachable or failed; outcome unknown: {0}")]
    Unavailable(String),
    /// A response that does not have the documented shape.
    #[error("unexpected Kite response: {0}")]
    Unexpected(String),
    /// No session: the owner has not logged in with Zerodha.
    #[error("not logged in with Zerodha")]
    NotLoggedIn,
}

/// Kite exception names after which nothing was done.
const REFUSALS: [&str; 5] = [
    "InputException",
    "OrderException",
    "MarginException",
    "HoldingException",
    "UserException",
];

/// A Kite Connect client for one API key and, once logged in, one session.
#[derive(Clone)]
pub struct KiteClient {
    http: reqwest::Client,
    base: String,
    api_key: String,
    access_token: Option<String>,
}

impl std::fmt::Debug for KiteClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KiteClient")
            .field("base", &self.base)
            .field("logged_in", &self.access_token.is_some())
            .finish_non_exhaustive()
    }
}

fn envelope(status: reqwest::StatusCode, body: &str) -> Result<Value, KiteError> {
    let parsed: Result<Value, _> = serde_json::from_str(body);
    let Ok(json) = parsed else {
        return Err(if status.is_server_error() {
            KiteError::Unavailable(format!("HTTP {status}"))
        } else {
            KiteError::Unexpected(format!("HTTP {status}: not JSON"))
        });
    };
    if json.get("status").and_then(Value::as_str) == Some("success") && status.is_success() {
        return Ok(json.get("data").cloned().unwrap_or(Value::Null));
    }
    let kind = json
        .get("error_type")
        .and_then(Value::as_str)
        .unwrap_or("GeneralException")
        .to_owned();
    let message = json
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("no message")
        .to_owned();
    Err(if kind == "TokenException" {
        KiteError::Token(message)
    } else if REFUSALS.contains(&kind.as_str()) && !status.is_server_error() {
        KiteError::Refused { kind, message }
    } else {
        KiteError::Unavailable(format!("{kind}: {message}"))
    })
}

fn transport(e: &reqwest::Error) -> KiteError {
    // The error text may contain the URL but never the headers.
    KiteError::Unavailable(e.to_string())
}

impl KiteClient {
    /// A client for `base` (use [`API_BASE`]) with a request timeout.
    pub fn new(
        base: &str,
        api_key: &str,
        access_token: Option<String>,
        timeout: Duration,
    ) -> Result<Self, KiteError> {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| KiteError::Unexpected(e.to_string()))?;
        Ok(Self {
            http,
            base: base.trim_end_matches('/').to_owned(),
            api_key: api_key.to_owned(),
            access_token: access_token.filter(|t| !t.is_empty()),
        })
    }

    /// The API key.
    #[must_use]
    pub fn api_key(&self) -> &str {
        &self.api_key
    }

    /// Whether the client holds a session.
    #[must_use]
    pub const fn logged_in(&self) -> bool {
        self.access_token.is_some()
    }

    fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        auth: bool,
    ) -> Result<reqwest::RequestBuilder, KiteError> {
        let mut builder = self
            .http
            .request(method, format!("{}{path}", self.base))
            .header("X-Kite-Version", "3");
        if auth {
            let token = self.access_token.as_ref().ok_or(KiteError::NotLoggedIn)?;
            builder = builder.header("Authorization", format!("token {}:{token}", self.api_key));
        }
        Ok(builder)
    }

    async fn send(&self, builder: reqwest::RequestBuilder) -> Result<Value, KiteError> {
        let response = builder.send().await.map_err(|e| transport(&e))?;
        let status = response.status();
        let body = response.text().await.map_err(|e| transport(&e))?;
        envelope(status, &body)
    }

    /// `GET path?query` with the session.
    pub async fn get(&self, path: &str, query: &[(&str, String)]) -> Result<Value, KiteError> {
        let builder = self.request(reqwest::Method::GET, path, true)?.query(query);
        self.send(builder).await
    }

    /// `POST path` with a form body; `auth` false only for the token exchange.
    pub async fn post_form(
        &self,
        path: &str,
        form: &[(&str, String)],
        auth: bool,
    ) -> Result<Value, KiteError> {
        let builder = self.request(reqwest::Method::POST, path, auth)?.form(form);
        self.send(builder).await
    }

    /// `DELETE path` with the session.
    pub async fn delete(&self, path: &str) -> Result<Value, KiteError> {
        let builder = self.request(reqwest::Method::DELETE, path, true)?;
        self.send(builder).await
    }

    /// `GET path` returning the raw body (the instruments CSV, gzip-decoded).
    pub async fn get_text(&self, path: &str) -> Result<String, KiteError> {
        let response = self
            .request(reqwest::Method::GET, path, true)?
            .send()
            .await
            .map_err(|e| transport(&e))?;
        let status = response.status();
        let body = response.text().await.map_err(|e| transport(&e))?;
        if status.is_success() {
            Ok(body)
        } else {
            envelope(status, &body).map(|_| String::new())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_are_classified_by_what_is_known_about_the_outcome() {
        let err = |status: u16, body: &str| {
            envelope(reqwest::StatusCode::from_u16(status).unwrap(), body).unwrap_err()
        };
        assert!(matches!(
            err(
                403,
                r#"{"status":"error","message":"expired","error_type":"TokenException"}"#
            ),
            KiteError::Token(_)
        ));
        assert!(matches!(
            err(
                400,
                r#"{"status":"error","message":"margin","error_type":"MarginException"}"#
            ),
            KiteError::Refused { .. }
        ));
        // Kite could not reach its order system: the order may or may not exist.
        assert!(matches!(
            err(
                503,
                r#"{"status":"error","message":"oms","error_type":"NetworkException"}"#
            ),
            KiteError::Unavailable(_)
        ));
        assert!(matches!(err(502, "<html>"), KiteError::Unavailable(_)));
        assert!(matches!(
            err(
                500,
                r#"{"status":"error","message":"x","error_type":"GeneralException"}"#
            ),
            KiteError::Unavailable(_)
        ));
        assert_eq!(
            envelope(
                reqwest::StatusCode::OK,
                r#"{"status":"success","data":{"a":1}}"#
            )
            .unwrap(),
            serde_json::json!({"a": 1})
        );
    }
}
