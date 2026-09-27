//! Health endpoints: `/health` (liveness), `/ready` (readiness and halt
//! state) and `/metrics` (Prometheus text; keep it off the public proxy).

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use chrono::Utc;
use qd_app::ports::HaltStore;
use serde::Serialize;
use sqlx::PgPool;

use crate::startup::{HealthCheck, check_database};

/// Shared state for the health routes.
#[derive(Clone)]
pub struct HealthState {
    /// Database pool.
    pub pool: PgPool,
    /// Kill-switch state.
    pub halts: Arc<dyn HaltStore>,
    /// The paper book, when paper trading is configured.
    pub paper: Option<Arc<dyn qd_app::ports::PaperTrading>>,
    /// The live book, when live trading is configured.
    pub live: Option<Arc<dyn qd_app::ports::PaperTrading>>,
    /// Alert thresholds and the paper book's calendar.
    pub monitor: qd_app::monitor::MonitorSettings,
    /// Where raised alerts are forwarded (Telegram, when switched on).
    pub notifier: Option<Arc<crate::notify::TelegramNotifier>>,
}

#[derive(Serialize)]
struct ActiveHalt {
    kind: qd_domain::halt::HaltKind,
    scope: qd_domain::halt::HaltScope,
    reason: String,
}

#[derive(Serialize)]
struct Readiness {
    ready: bool,
    checks: Vec<HealthCheck>,
    entries_halted: bool,
    active_halts: Vec<ActiveHalt>,
}

async fn health() -> &'static str {
    "ok"
}

async fn ready(State(state): State<HealthState>) -> impl IntoResponse {
    let now = Utc::now();
    let database = check_database(&state.pool).await;
    let (halt_check, active_halts, entries_halted) = match state.halts.load().await {
        Ok(halts) => {
            let active: Vec<ActiveHalt> = halts
                .iter()
                .filter(|h| h.is_active_at(now))
                .map(|h| ActiveHalt {
                    kind: h.kind(),
                    scope: h.scope(),
                    reason: h.reason().to_owned(),
                })
                .collect();
            let halted = !active.is_empty();
            (
                HealthCheck {
                    name: "halt_store",
                    ok: true,
                    detail: "readable".to_owned(),
                },
                active,
                halted,
            )
        }
        // Unknown kill-switch state counts as halted (INV-06).
        Err(e) => (
            HealthCheck {
                name: "halt_store",
                ok: false,
                detail: e.to_string(),
            },
            Vec::new(),
            true,
        ),
    };
    let checks = vec![database, halt_check];
    let ok = checks.iter().all(|c| c.ok);
    let status = if ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        status,
        Json(Readiness {
            ready: ok,
            checks,
            entries_halted,
            active_halts,
        }),
    )
}

async fn metrics(State(state): State<HealthState>) -> impl IntoResponse {
    let health = qd_app::monitor::collect(
        state.halts.as_ref(),
        state.paper.as_deref(),
        state.live.as_deref(),
        Utc::now(),
        state.monitor.clone(),
    )
    .await;
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        qd_app::monitor::prometheus(&health),
    )
}

/// The health router: `/health`, `/ready` and `/metrics`.
pub fn router(state: HealthState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/metrics", get(metrics))
        .with_state(state)
}

/// Logs alert changes as structured events every `interval`: a raised
/// critical alert at error level, a warning at warn level, a cleared one at
/// info. Raised alerts also go to Telegram when notifications are on.
pub fn spawn_alert_log(state: HealthState, interval: std::time::Duration) {
    tokio::spawn(async move {
        let mut previous: std::collections::HashSet<qd_app::monitor::Alert> =
            std::collections::HashSet::new();
        loop {
            let health = qd_app::monitor::collect(
                state.halts.as_ref(),
                state.paper.as_deref(),
                state.live.as_deref(),
                Utc::now(),
                state.monitor.clone(),
            )
            .await;
            let current: std::collections::HashSet<_> = health.alerts.into_iter().collect();
            for alert in current.difference(&previous) {
                match alert.severity {
                    qd_app::monitor::Severity::Critical => {
                        tracing::error!(alert = alert.code, message = %alert.message, "alert raised");
                    }
                    qd_app::monitor::Severity::Warning => {
                        tracing::warn!(alert = alert.code, message = %alert.message, "alert raised");
                    }
                }
                if let Some(n) = &state.notifier {
                    n.alert(alert).await;
                }
            }
            for alert in previous.difference(&current) {
                tracing::info!(alert = alert.code, "alert cleared");
            }
            previous = current;
            tokio::time::sleep(interval).await;
        }
    });
}
