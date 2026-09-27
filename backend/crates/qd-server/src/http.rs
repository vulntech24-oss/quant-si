//! Health endpoints. The full API arrives with `qd-api` (Phase 5).

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

/// The health router: `/health` (liveness) and `/ready` (readiness and halt state).
pub fn router(state: HealthState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .with_state(state)
}
