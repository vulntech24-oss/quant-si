//! QuantDesk HTTP API (spec §5.3 `qd-api`).
//!
//! Handlers do three things only: parse and validate the request, authorize
//! the caller, call a use case or port and map the result. The API depends
//! on `qd-app` ports; it never touches SQL or broker adapters, and never
//! talks to Kite or AI providers (INV-15).

pub mod auth;
pub mod dto;
pub mod routes;
pub mod totp;

use std::sync::Arc;

use axum::Json;
use axum::extract::Request;
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use qd_app::live::Environment;
use qd_app::ports::{
    AccountStore, AuditLog, AuthStore, BacktestRunner, Clock, HaltStore, HistoricalMarketData,
    JournalReader, PaperTrading,
};
use qd_app::registry::StrategyRegistry;
use qd_domain::ids::AccountId;
use serde::Serialize;
use thiserror::Error;

pub use routes::router;

/// Settings the API needs.
#[derive(Clone, Copy, Debug)]
pub struct ApiSettings {
    /// The account this deployment operates.
    pub account_id: AccountId,
    /// Environment.
    pub environment: Environment,
    /// Live-trading switch from configuration.
    pub live_trading_enabled: bool,
    /// Whether the build has the `live-orders` feature.
    pub live_orders_compiled: bool,
    /// Send cookies with `Secure` (true in production behind TLS).
    pub secure_cookies: bool,
    /// Session lifetime in hours.
    pub session_hours: i64,
    /// The live account, when live trading is configured (ADR 0014).
    pub live_account_id: Option<AccountId>,
}

/// Everything the handlers use.
#[derive(Clone)]
pub struct ApiState {
    /// Users and sessions.
    pub auth: Arc<dyn AuthStore>,
    /// Journal reader.
    pub journal: Arc<dyn JournalReader>,
    /// Kill switch.
    pub halts: Arc<dyn HaltStore>,
    /// Strategy registry.
    pub registry: StrategyRegistry,
    /// Instruments and bars.
    pub market: Arc<dyn HistoricalMarketData>,
    /// Accounts.
    pub accounts: Arc<dyn AccountStore>,
    /// Audit log.
    pub audit: Arc<dyn AuditLog>,
    /// Research backtests.
    pub backtests: Arc<dyn BacktestRunner>,
    /// Paper trading, when configured.
    pub paper: Option<Arc<dyn PaperTrading>>,
    /// Strategy validation.
    pub validator: Arc<dyn qd_app::ports::Validator>,
    /// Recorded evidence.
    pub evidence: Arc<dyn qd_app::evidence::EvidenceStore>,
    /// Advisory AI, when enabled (INV-04).
    pub ai: Option<Arc<dyn qd_app::ports::AiAdvisory>>,
    /// Settings saved from the web UI (ADR 0013).
    pub settings_admin: Arc<dyn qd_app::ports::SettingsAdmin>,
    /// Write-only secrets (INV-15).
    pub secrets: Arc<dyn qd_app::ports::SecretStore>,
    /// The live book (Zerodha), when configured.
    pub live: Option<Arc<dyn PaperTrading>>,
    /// The broker connection (Zerodha Kite login, data, fills).
    pub broker: Option<Arc<dyn qd_app::ports::BrokerLink>>,
    /// Walk-forward parameter searches (research only).
    pub search: Option<Arc<dyn qd_app::ports::ParameterSearch>>,
    /// TOTP second factors (encrypted with the master key).
    pub totp: Option<Arc<dyn qd_app::ports::TotpStore>>,
    /// Portfolio views of the paper and live books.
    pub portfolio: Option<Arc<dyn qd_app::ports::PortfolioReader>>,
    /// Instruments and bar uploads from the web UI.
    pub data: Option<Arc<dyn qd_app::ports::DataAdmin>>,
    /// Alert thresholds and the paper book's calendar.
    pub monitor: qd_app::monitor::MonitorSettings,
    /// Owner notifications (Telegram).
    pub notifier: Option<Arc<dyn qd_app::ports::Notifier>>,
    /// Paper review and calibration.
    pub reviewer: Arc<dyn qd_app::review::Reviewer>,
    /// Clock.
    pub clock: Arc<dyn Clock>,
    /// Settings.
    pub settings: ApiSettings,
    /// Login throttling.
    pub limiter: Arc<auth::LoginLimiter>,
}

/// An API error. Messages never contain secrets or internal detail for auth failures.
#[derive(Debug, Error)]
pub enum ApiError {
    /// Not logged in, or the session expired.
    #[error("authentication required")]
    Unauthorized,
    /// Logged in, but not allowed.
    #[error("forbidden: {0}")]
    Forbidden(String),
    /// The action needs a recent password re-entry.
    #[error("step-up authentication required")]
    StepUpRequired,
    /// The password was right; the authenticator code is missing or wrong.
    #[error("authenticator code required")]
    TotpRequired,
    /// Invalid input.
    #[error("bad request: {0}")]
    BadRequest(String),
    /// Not found.
    #[error("not found")]
    NotFound,
    /// Too many attempts.
    #[error("too many attempts; try later")]
    TooManyRequests,
    /// The action conflicts with the current state (for example an illegal transition).
    #[error("conflict: {0}")]
    Conflict(String),
    /// Something failed on the server.
    #[error("internal error")]
    Internal(String),
}

#[derive(Serialize)]
struct ErrorBody {
    error: String,
    code: &'static str,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code) = match &self {
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
            Self::Forbidden(_) => (StatusCode::FORBIDDEN, "forbidden"),
            Self::StepUpRequired => (StatusCode::FORBIDDEN, "step_up_required"),
            Self::TotpRequired => (StatusCode::UNAUTHORIZED, "totp_required"),
            Self::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request"),
            Self::NotFound => (StatusCode::NOT_FOUND, "not_found"),
            Self::TooManyRequests => (StatusCode::TOO_MANY_REQUESTS, "too_many_requests"),
            Self::Conflict(_) => (StatusCode::CONFLICT, "conflict"),
            Self::Internal(detail) => {
                // The detail stays in the server log; the client sees "internal error".
                tracing::error!(detail = %detail, "api internal error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal")
            }
        };
        (
            status,
            Json(ErrorBody {
                error: self.to_string(),
                code,
            }),
        )
            .into_response()
    }
}

/// Rejects state-changing requests without the CSRF header.
pub async fn csrf_guard(request: Request, next: Next) -> Response {
    let safe = matches!(
        *request.method(),
        Method::GET | Method::HEAD | Method::OPTIONS
    );
    let ok = safe
        || request
            .headers()
            .get(auth::CSRF_HEADER)
            .is_some_and(|v| v == auth::CSRF_VALUE);
    if ok {
        next.run(request).await
    } else {
        ApiError::Forbidden("missing CSRF header".to_owned()).into_response()
    }
}

/// Adds security headers to every response.
pub async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    for (name, value) in [
        (
            header::CONTENT_SECURITY_POLICY,
            "default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'",
        ),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::X_FRAME_OPTIONS, "DENY"),
        (header::REFERRER_POLICY, "no-referrer"),
        (header::CACHE_CONTROL, "no-store"),
    ] {
        headers.insert(name, HeaderValue::from_static(value));
    }
    response
}
