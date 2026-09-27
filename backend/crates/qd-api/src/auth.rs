//! Authentication and authorization (ADR 0008).
//!
//! - Passwords: argon2id PHC strings.
//! - Sessions: a random 256-bit token in an `HttpOnly`, `SameSite=Strict`
//!   cookie (`Secure` in production); only its SHA-256 is stored.
//! - Roles: the owner may act; viewers may only read.
//! - Step-up: dangerous actions (re-arming halts, promotions, live arming)
//!   need a password re-entry within the last five minutes.
//! - CSRF: mutating requests must carry `X-Requested-With: quantdesk`, which a
//!   cross-site form cannot send; together with `SameSite=Strict`.
//! - Login throttling: five failures per username per 15 minutes.

use std::collections::HashMap;
use std::sync::Mutex;

use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{HeaderMap, header};
use chrono::{DateTime, Duration, Utc};
use qd_app::ports::{Role, SessionRecord, UserRecord};
use sha2::{Digest, Sha256};

use crate::{ApiError, ApiState};

/// Session cookie name.
pub const SESSION_COOKIE: &str = "qd_session";
/// The CSRF header and its required value.
pub const CSRF_HEADER: &str = "x-requested-with";
/// Required value of [`CSRF_HEADER`].
pub const CSRF_VALUE: &str = "quantdesk";
/// How long a step-up lasts.
pub const STEP_UP_MINUTES: i64 = 5;

/// Hashes a password with argon2id and a random salt.
pub fn hash_password(password: &str) -> Result<String, ApiError> {
    if password.chars().count() < 12 {
        return Err(ApiError::BadRequest(
            "password must be at least 12 characters".to_owned(),
        ));
    }
    let mut salt_bytes = [0_u8; 16];
    getrandom::fill(&mut salt_bytes).map_err(|e| ApiError::Internal(e.to_string()))?;
    let salt =
        SaltString::encode_b64(&salt_bytes).map_err(|e| ApiError::Internal(e.to_string()))?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| ApiError::Internal(e.to_string()))
}

/// Verifies a password against a stored hash in constant time.
#[must_use]
pub fn verify_password(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash).is_ok_and(|parsed| {
        Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok()
    })
}

/// A new random session token (hex, 256 bits).
pub fn new_token() -> Result<String, ApiError> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(hex::encode(bytes))
}

/// SHA-256 of a token, hex. Only this is stored.
#[must_use]
pub fn token_hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// The session cookie for a new login.
#[must_use]
pub fn session_cookie(token: &str, max_age_seconds: i64, secure: bool) -> String {
    let secure = if secure { "; Secure" } else { "" };
    format!(
        "{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age_seconds}{secure}"
    )
}

/// A cookie that deletes the session cookie.
#[must_use]
pub fn clear_cookie(secure: bool) -> String {
    session_cookie("", 0, secure)
}

fn cookie_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == SESSION_COOKIE)
        .map(|(_, value)| value.to_owned())
        .filter(|v| !v.is_empty())
}

/// An authenticated caller.
#[derive(Clone, Debug)]
pub struct Caller {
    /// The user.
    pub user: UserRecord,
    /// The session.
    pub session: SessionRecord,
}

impl Caller {
    /// Fails unless the caller is the owner.
    pub fn require_owner(&self) -> Result<(), ApiError> {
        if self.user.role == Role::Owner {
            Ok(())
        } else {
            Err(ApiError::Forbidden("owner only".to_owned()))
        }
    }

    /// Fails unless the owner re-authenticated recently.
    pub fn require_step_up(&self, now: DateTime<Utc>) -> Result<(), ApiError> {
        self.require_owner()?;
        if self
            .session
            .stepped_up_until
            .is_some_and(|until| now < until)
        {
            Ok(())
        } else {
            Err(ApiError::StepUpRequired)
        }
    }

    /// Name recorded in the audit log.
    #[must_use]
    pub fn actor(&self) -> String {
        format!("user:{}", self.user.username)
    }
}

impl FromRequestParts<ApiState> for Caller {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &ApiState,
    ) -> Result<Self, Self::Rejection> {
        let token = cookie_token(&parts.headers).ok_or(ApiError::Unauthorized)?;
        let hash = token_hash(&token);
        let session = state
            .auth
            .session(&hash)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?
            .ok_or(ApiError::Unauthorized)?;
        if session.expires_at <= state.clock.now() {
            return Err(ApiError::Unauthorized);
        }
        let user = state
            .auth
            .user(session.user)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?
            .ok_or(ApiError::Unauthorized)?;
        Ok(Self { user, session })
    }
}

/// Throttles failed logins per username.
#[derive(Debug, Default)]
pub struct LoginLimiter {
    failures: Mutex<HashMap<String, Vec<DateTime<Utc>>>>,
}

/// Failures allowed per window.
const MAX_FAILURES: usize = 5;
/// The window.
const WINDOW_MINUTES: i64 = 15;

impl LoginLimiter {
    /// Whether this username may try now.
    #[must_use]
    pub fn allowed(&self, username: &str, now: DateTime<Utc>) -> bool {
        let mut failures = self
            .failures
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let list = failures.entry(username.to_owned()).or_default();
        list.retain(|t| now - *t < Duration::minutes(WINDOW_MINUTES));
        list.len() < MAX_FAILURES
    }

    /// Records a failure.
    pub fn failed(&self, username: &str, now: DateTime<Utc>) {
        self.failures
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(username.to_owned())
            .or_default()
            .push(now);
    }

    /// Clears failures after a success.
    pub fn succeeded(&self, username: &str) {
        self.failures
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(username);
    }
}
