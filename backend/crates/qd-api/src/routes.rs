//! Routes. See `openapi.yaml` in this crate for the contract.

use std::convert::Infallible;
use std::time::Duration as StdDuration;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::middleware;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{Duration, NaiveDate};
use futures_util::stream::{self, Stream};
use qd_app::ports::{BacktestRequest, SessionRecord};
use qd_app::registry::RegistryError;
use qd_domain::halt::{ClearedBy, Halt, HaltKind, HaltScope};
use qd_domain::ids::{DecisionId, EvidenceId, HaltId, InstrumentId, StrategyVersionId};
use qd_domain::lifecycle::strategy::{OwnerApproval, StageEvent, TradingStage};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::auth::{self, Caller, STEP_UP_MINUTES};
use crate::dto::{DecisionSummary, decision_summary};
use crate::{ApiError, ApiState, csrf_guard, security_headers};

/// The OpenAPI contract, served at `/api/openapi.yaml`.
pub const OPENAPI: &str = include_str!("../openapi.yaml");

fn internal(e: impl std::fmt::Display) -> ApiError {
    ApiError::Internal(e.to_string())
}

/// The API router, mounted at `/api`.
pub fn router(state: ApiState) -> Router {
    let api = Router::new()
        .route("/auth/login", post(login))
        .route("/auth/logout", post(logout))
        .route("/auth/me", get(me))
        .route("/auth/step-up", post(step_up))
        .route("/auth/totp", get(totp_status))
        .route("/auth/totp/setup", post(totp_setup))
        .route("/auth/totp/enable", post(totp_enable))
        .route("/auth/totp/disable", post(totp_disable))
        .route("/auth/sessions", get(sessions_list))
        .route("/auth/sessions/revoke-all", post(sessions_revoke_all))
        .route("/auth/sessions/{id}", axum::routing::delete(session_revoke))
        .route("/status", get(status))
        .route("/decisions", get(decisions))
        .route("/decisions/{id}", get(decision))
        .route("/journal", get(journal))
        .route("/halts", get(halts).post(create_halt))
        .route("/halts/{id}/rearm", post(rearm_halt))
        .route("/strategies", get(strategies))
        .route("/strategies/{id}/events", post(strategy_event))
        .route("/strategy-catalog", get(strategy_catalog))
        .route("/instruments", get(instruments).post(add_instrument))
        .route("/instruments/{id}/bars", get(bars).post(import_bars))
        .route("/instruments/{id}/quality", get(bar_quality))
        .route("/backtests", post(backtest))
        .route("/validations", post(run_validation))
        .route("/evidence", get(evidence_list))
        .route("/evidence/{id}", get(evidence_one))
        .route("/ai/run", post(ai_run))
        .route("/ai/advice", get(ai_advice))
        .route("/ai/scorecard", get(ai_scorecard))
        .route("/review", get(review))
        .route("/review/{version}/record", post(record_review))
        .route("/settings", get(settings_view))
        .route("/settings/{section}", axum::routing::put(settings_update))
        .route("/settings/{section}/reset", post(settings_reset))
        .route("/settings/{section}/history", get(settings_history))
        .route(
            "/settings/{section}/restore/{version}",
            post(settings_restore),
        )
        .route("/secrets/rotate-key", post(rotate_master_key))
        .route("/secrets", get(secrets_status))
        .route(
            "/secrets/{name}",
            axum::routing::put(secret_set).delete(secret_clear),
        )
        .route("/paper", get(paper_state))
        .route("/paper/run", post(paper_run))
        .route("/account/live-armed", post(live_armed))
        .route("/kite", get(kite_status))
        .route("/kite/login", post(kite_login))
        .route("/kite/callback", get(kite_callback))
        .route("/kite/sync-bars", post(kite_sync_bars))
        .route("/kite/sync-fills", post(kite_sync_fills))
        .route("/live", get(live_state))
        .route("/portfolio", get(portfolio_view))
        .route("/live/run", post(live_run))
        .route("/notifications/test", post(notifications_test))
        .route("/events", get(events))
        .route("/openapi.yaml", get(openapi))
        .layer(middleware::from_fn(csrf_guard))
        .with_state(state);
    Router::new()
        .nest("/api", api)
        .layer(middleware::from_fn(security_headers))
}

// ---------- auth ----------

#[derive(Deserialize)]
struct LoginRequest {
    username: String,
    password: String,
    /// The authenticator code, when the account has TOTP enabled.
    #[serde(default)]
    code: Option<String>,
}

#[derive(Serialize)]
struct MeResponse {
    username: String,
    role: qd_app::ports::Role,
    stepped_up_until: Option<chrono::DateTime<chrono::Utc>>,
}

async fn login(
    State(state): State<ApiState>,
    Json(body): Json<LoginRequest>,
) -> Result<Response, ApiError> {
    let now = state.clock.now();
    if !state.limiter.allowed(&body.username, now) {
        return Err(ApiError::TooManyRequests);
    }
    let user = state
        .auth
        .user_by_name(&body.username)
        .await
        .map_err(internal)?;
    let Some(user) = user.filter(|u| auth::verify_password(&body.password, &u.password_hash))
    else {
        state.limiter.failed(&body.username, now);
        let _ = state
            .audit
            .record(
                "anonymous",
                "auth.login_failed",
                json!({ "username": body.username }),
            )
            .await;
        return Err(ApiError::Unauthorized);
    };
    // Second factor (ADR 0015): with TOTP enabled, the password alone is not
    // enough. A missing or wrong code counts as a failed attempt.
    if let Some(store) = &state.totp {
        if let Some(record) = store.totp(user.id).await.map_err(internal)? {
            if record.enabled {
                let step = body
                    .code
                    .as_deref()
                    .and_then(|c| crate::totp::verify(record.secret.expose(), c, now));
                let accepted = match step {
                    Some(step) => store.use_totp_step(user.id, step).await.map_err(internal)?,
                    None => false,
                };
                if !accepted {
                    state.limiter.failed(&body.username, now);
                    let _ = state
                        .audit
                        .record(
                            "anonymous",
                            "auth.totp_failed",
                            json!({ "username": body.username, "code_given": body.code.is_some() }),
                        )
                        .await;
                    return Err(ApiError::TotpRequired);
                }
            }
        }
    }
    state.limiter.succeeded(&body.username);
    let token = auth::new_token()?;
    let session = SessionRecord {
        token_hash: auth::token_hash(&token),
        user: user.id,
        expires_at: now + Duration::hours(state.settings.session_hours),
        stepped_up_until: None,
    };
    state
        .auth
        .create_session(&session)
        .await
        .map_err(internal)?;
    state
        .audit
        .record(&format!("user:{}", user.username), "auth.login", json!({}))
        .await
        .map_err(internal)?;
    let cookie = auth::session_cookie(
        &token,
        state.settings.session_hours * 3600,
        state.settings.secure_cookies,
    );
    let mut response = Json(MeResponse {
        username: user.username,
        role: user.role,
        stepped_up_until: None,
    })
    .into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie).map_err(internal)?,
    );
    Ok(response)
}

async fn logout(State(state): State<ApiState>, caller: Caller) -> Result<Response, ApiError> {
    state
        .auth
        .delete_session(&caller.session.token_hash)
        .await
        .map_err(internal)?;
    state
        .audit
        .record(&caller.actor(), "auth.logout", json!({}))
        .await
        .map_err(internal)?;
    let mut response = Json(json!({ "ok": true })).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&auth::clear_cookie(state.settings.secure_cookies))
            .map_err(internal)?,
    );
    Ok(response)
}

async fn me(caller: Caller) -> Json<MeResponse> {
    Json(MeResponse {
        username: caller.user.username,
        role: caller.user.role,
        stepped_up_until: caller.session.stepped_up_until,
    })
}

#[derive(Deserialize)]
struct StepUpRequest {
    password: String,
}

async fn step_up(
    State(state): State<ApiState>,
    caller: Caller,
    Json(body): Json<StepUpRequest>,
) -> Result<Json<Value>, ApiError> {
    caller.require_owner()?;
    let now = state.clock.now();
    if !state.limiter.allowed(&caller.user.username, now) {
        return Err(ApiError::TooManyRequests);
    }
    if !auth::verify_password(&body.password, &caller.user.password_hash) {
        state.limiter.failed(&caller.user.username, now);
        return Err(ApiError::Unauthorized);
    }
    let until = now + Duration::minutes(STEP_UP_MINUTES);
    state
        .auth
        .step_up(&caller.session.token_hash, until)
        .await
        .map_err(internal)?;
    state
        .audit
        .record(&caller.actor(), "auth.step_up", json!({}))
        .await
        .map_err(internal)?;
    Ok(Json(json!({ "stepped_up_until": until })))
}

// ---------- second factor and sessions (ADR 0015) ----------

fn totp_store(state: &ApiState) -> Result<&std::sync::Arc<dyn qd_app::ports::TotpStore>, ApiError> {
    state
        .totp
        .as_ref()
        .ok_or_else(|| ApiError::Conflict("two-factor login is not available".to_owned()))
}

async fn totp_status(
    State(state): State<ApiState>,
    caller: Caller,
) -> Result<Json<Value>, ApiError> {
    let record = match &state.totp {
        Some(store) => store.totp(caller.user.id).await.map_err(internal)?,
        None => None,
    };
    Ok(Json(json!({
        "available": state.totp.is_some(),
        "enabled": record.as_ref().is_some_and(|r| r.enabled),
        "pending": record.as_ref().is_some_and(|r| !r.enabled),
    })))
}

/// Starts enrollment: a new secret, shown once, to add to an authenticator app.
async fn totp_setup(
    State(state): State<ApiState>,
    caller: Caller,
) -> Result<Json<Value>, ApiError> {
    caller.require_step_up(state.clock.now())?;
    let store = totp_store(&state)?;
    if store
        .totp(caller.user.id)
        .await
        .map_err(internal)?
        .is_some_and(|r| r.enabled)
    {
        return Err(ApiError::Conflict(
            "two-factor login is already on; turn it off first".to_owned(),
        ));
    }
    let secret = crate::totp::new_secret().map_err(internal)?;
    store
        .put_pending_totp(
            caller.user.id,
            &qd_app::ports::SecretValue::new(secret.clone()),
        )
        .await
        .map_err(|e| ApiError::Conflict(e.0))?;
    state
        .audit
        .record(&caller.actor(), "auth.totp_setup", json!({}))
        .await
        .map_err(internal)?;
    let uri = crate::totp::otpauth_uri("QuantDesk", &caller.user.username, &secret);
    Ok(Json(json!({ "secret": secret, "uri": uri })))
}

#[derive(Deserialize)]
struct TotpCode {
    code: String,
}

/// Checks a code against the stored secret and consumes its time step.
async fn check_code(
    state: &ApiState,
    user: qd_domain::ids::UserId,
    code: &str,
) -> Result<qd_app::ports::TotpRecord, ApiError> {
    let store = totp_store(state)?;
    let record = store
        .totp(user)
        .await
        .map_err(internal)?
        .ok_or_else(|| ApiError::Conflict("no authenticator is set up".to_owned()))?;
    let step = crate::totp::verify(record.secret.expose(), code, state.clock.now())
        .ok_or_else(|| ApiError::BadRequest("wrong authenticator code".to_owned()))?;
    if !store.use_totp_step(user, step).await.map_err(internal)? {
        return Err(ApiError::BadRequest(
            "that code was already used; wait for the next one".to_owned(),
        ));
    }
    Ok(record)
}

async fn totp_enable(
    State(state): State<ApiState>,
    caller: Caller,
    Json(body): Json<TotpCode>,
) -> Result<Json<Value>, ApiError> {
    caller.require_step_up(state.clock.now())?;
    let record = check_code(&state, caller.user.id, &body.code).await?;
    if record.enabled {
        return Err(ApiError::Conflict("already on".to_owned()));
    }
    totp_store(&state)?
        .enable_totp(caller.user.id)
        .await
        .map_err(internal)?;
    state
        .audit
        .record(&caller.actor(), "auth.totp_enabled", json!({}))
        .await
        .map_err(internal)?;
    Ok(Json(json!({ "enabled": true })))
}

async fn totp_disable(
    State(state): State<ApiState>,
    caller: Caller,
    Json(body): Json<TotpCode>,
) -> Result<Json<Value>, ApiError> {
    caller.require_step_up(state.clock.now())?;
    check_code(&state, caller.user.id, &body.code).await?;
    totp_store(&state)?
        .remove_totp(caller.user.id)
        .await
        .map_err(internal)?;
    state
        .audit
        .record(&caller.actor(), "auth.totp_disabled", json!({}))
        .await
        .map_err(internal)?;
    Ok(Json(json!({ "enabled": false })))
}

/// A session's public id: the start of its token hash.
fn session_id(token_hash: &str) -> String {
    token_hash.chars().take(12).collect()
}

async fn sessions_list(
    State(state): State<ApiState>,
    caller: Caller,
) -> Result<Json<Value>, ApiError> {
    let sessions = state
        .auth
        .sessions_for(caller.user.id, state.clock.now())
        .await
        .map_err(internal)?;
    Ok(Json(Value::Array(
        sessions
            .iter()
            .map(|s| {
                json!({
                    "id": session_id(&s.token_hash),
                    "created_at": s.created_at,
                    "expires_at": s.expires_at,
                    "current": s.token_hash == caller.session.token_hash,
                })
            })
            .collect(),
    )))
}

/// "Log out everywhere": every session of the caller, this one included.
async fn sessions_revoke_all(
    State(state): State<ApiState>,
    caller: Caller,
) -> Result<Response, ApiError> {
    let removed = state
        .auth
        .delete_user_sessions(caller.user.id)
        .await
        .map_err(internal)?;
    state
        .audit
        .record(
            &caller.actor(),
            "auth.logout_everywhere",
            json!({ "sessions": removed }),
        )
        .await
        .map_err(internal)?;
    let mut response = Json(json!({ "revoked": removed })).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&auth::clear_cookie(state.settings.secure_cookies))
            .map_err(internal)?,
    );
    Ok(response)
}

async fn session_revoke(
    State(state): State<ApiState>,
    caller: Caller,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let sessions = state
        .auth
        .sessions_for(caller.user.id, state.clock.now())
        .await
        .map_err(internal)?;
    let target = sessions
        .iter()
        .find(|s| session_id(&s.token_hash) == id)
        .ok_or(ApiError::NotFound)?;
    state
        .auth
        .delete_session(&target.token_hash)
        .await
        .map_err(internal)?;
    state
        .audit
        .record(
            &caller.actor(),
            "auth.session_revoked",
            json!({ "session": id }),
        )
        .await
        .map_err(internal)?;
    Ok(Json(json!({ "revoked": id })))
}

// ---------- status ----------

#[derive(Serialize)]
struct HaltView {
    id: HaltId,
    kind: HaltKind,
    scope: HaltScope,
    reason: String,
    started_at: chrono::DateTime<chrono::Utc>,
    requires_manual_rearm: bool,
    active: bool,
}

fn halt_view(h: &Halt, now: chrono::DateTime<chrono::Utc>) -> HaltView {
    HaltView {
        id: h.id(),
        kind: h.kind(),
        scope: h.scope(),
        reason: h.reason().to_owned(),
        started_at: h.started_at(),
        requires_manual_rearm: h.requires_manual_rearm(),
        active: h.is_active_at(now),
    }
}

async fn status(State(state): State<ApiState>, _caller: Caller) -> Result<Json<Value>, ApiError> {
    let now = state.clock.now();
    let account = state
        .accounts
        .account(state.settings.account_id)
        .await
        .map_err(internal)?;
    let (known, active): (bool, Vec<HaltView>) = match state.halts.load().await {
        Ok(halts) => (
            true,
            halts
                .iter()
                .filter(|h| h.is_active_at(now))
                .map(|h| halt_view(h, now))
                .collect(),
        ),
        Err(_) => (false, Vec::new()),
    };
    let live_account = match state.settings.live_account_id {
        Some(id) => state.accounts.account(id).await.map_err(internal)?,
        None => None,
    };
    // Unknown halt state counts as halted (INV-06).
    let entries_halted = !known || !active.is_empty();
    let health = qd_app::monitor::collect(
        state.halts.as_ref(),
        state.paper.as_deref(),
        state.live.as_deref(),
        now,
        state.monitor.clone(),
    )
    .await;
    Ok(Json(json!({
        "alerts": health.alerts,
        "paper": health.paper,
        "live": health.live,
        "live_account": live_account,
        "now": now,
        "environment": state.settings.environment,
        "account": account,
        "live_trading_enabled": state.settings.live_trading_enabled,
        "live_orders_compiled": state.settings.live_orders_compiled,
        "halt_state_known": known,
        "entries_halted": entries_halted,
        "active_halts": active,
    })))
}

// ---------- decisions and journal ----------

#[derive(Deserialize)]
struct Page {
    limit: Option<i64>,
    before: Option<i64>,
    kind: Option<String>,
}

async fn decisions(
    State(state): State<ApiState>,
    _caller: Caller,
    Query(page): Query<Page>,
) -> Result<Json<Vec<DecisionSummary>>, ApiError> {
    let entries = state
        .journal
        .recent(Some("decision"), page.before, page.limit.unwrap_or(50))
        .await
        .map_err(internal)?;
    Ok(Json(entries.iter().filter_map(decision_summary).collect()))
}

async fn decision(
    State(state): State<ApiState>,
    _caller: Caller,
    Path(id): Path<DecisionId>,
) -> Result<Json<Value>, ApiError> {
    let stored = state
        .journal
        .decision(id)
        .await
        .map_err(internal)?
        .ok_or(ApiError::NotFound)?;
    let summary = decision_summary(&stored).ok_or(ApiError::NotFound)?;
    Ok(Json(json!({ "summary": summary, "record": stored.entry })))
}

async fn journal(
    State(state): State<ApiState>,
    _caller: Caller,
    Query(page): Query<Page>,
) -> Result<Json<Value>, ApiError> {
    let entries = state
        .journal
        .recent(page.kind.as_deref(), page.before, page.limit.unwrap_or(50))
        .await
        .map_err(internal)?;
    Ok(Json(serde_json::to_value(entries).map_err(internal)?))
}

/// New journal entries as server-sent events, polled every two seconds.
async fn events(
    State(state): State<ApiState>,
    _caller: Caller,
    headers: HeaderMap,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let start = match headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
    {
        Some(seq) => seq,
        None => state
            .journal
            .recent(None, None, 1)
            .await
            .map_err(internal)?
            .first()
            .map_or(0, |e| e.seq),
    };
    let stream = stream::unfold((state, start), |(state, last)| async move {
        tokio::time::sleep(StdDuration::from_secs(2)).await;
        let entries = state.journal.after(last, 100).await.unwrap_or_default();
        let next = entries.last().map_or(last, |e| e.seq);
        let events: Vec<Result<Event, Infallible>> = entries
            .iter()
            .map(|e| {
                Ok(Event::default()
                    .id(e.seq.to_string())
                    .event(e.kind.clone())
                    .data(json!({ "seq": e.seq, "kind": e.kind }).to_string()))
            })
            .collect();
        Some((stream::iter(events), (state, next)))
    });
    Ok(Sse::new(futures_util::StreamExt::flatten(stream)).keep_alive(KeepAlive::default()))
}

// ---------- halts ----------

async fn halts(
    State(state): State<ApiState>,
    _caller: Caller,
) -> Result<Json<Vec<HaltView>>, ApiError> {
    let now = state.clock.now();
    let halts = state.halts.load().await.map_err(internal)?;
    Ok(Json(halts.iter().map(|h| halt_view(h, now)).collect()))
}

#[derive(Deserialize)]
struct CreateHalt {
    reason: String,
}

/// Owner-only; no step-up needed: halting only reduces risk.
async fn create_halt(
    State(state): State<ApiState>,
    caller: Caller,
    Json(body): Json<CreateHalt>,
) -> Result<Json<HaltView>, ApiError> {
    caller.require_owner()?;
    let now = state.clock.now();
    let halt = Halt::new(
        HaltId::new_at(now),
        HaltKind::Manual,
        HaltScope::Global,
        body.reason,
        now,
        None,
        true,
    )
    .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    state.halts.record(&halt).await.map_err(internal)?;
    state
        .audit
        .record(&caller.actor(), "halt.create", json!({ "halt": halt.id() }))
        .await
        .map_err(internal)?;
    Ok(Json(halt_view(&halt, now)))
}

/// Re-arming allows risk again: owner with a recent step-up (INV-07).
async fn rearm_halt(
    State(state): State<ApiState>,
    caller: Caller,
    Path(id): Path<HaltId>,
) -> Result<Json<HaltView>, ApiError> {
    let now = state.clock.now();
    caller.require_step_up(now)?;
    let halt = state
        .halts
        .load()
        .await
        .map_err(internal)?
        .into_iter()
        .find(|h| h.id() == id)
        .ok_or(ApiError::NotFound)?;
    let cleared = halt
        .clear(ClearedBy::Human(caller.user.id), now)
        .map_err(|e| ApiError::Conflict(e.to_string()))?;
    state.halts.record(&cleared).await.map_err(internal)?;
    state
        .audit
        .record(&caller.actor(), "halt.rearm", json!({ "halt": id }))
        .await
        .map_err(internal)?;
    Ok(Json(halt_view(&cleared, now)))
}

// ---------- strategies ----------

async fn strategies(
    State(state): State<ApiState>,
    _caller: Caller,
) -> Result<Json<Vec<Value>>, ApiError> {
    let versions = state.registry.versions().await.map_err(registry_error)?;
    let mut out = Vec::with_capacity(versions.len());
    for v in versions {
        let stage = state
            .registry
            .stage(v.reference.version_id)
            .await
            .map_err(registry_error)?;
        out.push(json!({ "version": v, "stage": stage }));
    }
    Ok(Json(out))
}

fn registry_error(e: RegistryError) -> ApiError {
    match e {
        RegistryError::UnknownVersion => ApiError::NotFound,
        RegistryError::IllegalTransition(m) => ApiError::Conflict(m),
        RegistryError::Evidence(m) => ApiError::Conflict(format!("evidence: {m}")),
        other => ApiError::Internal(other.to_string()),
    }
}

#[derive(Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum StrategyEventRequest {
    StartResearch,
    RejectResearch {
        reason: String,
    },
    PassResearch {
        evidence: EvidenceId,
    },
    Promote {
        to: TradingStage,
        evidence: EvidenceId,
    },
    Suspend {
        reason: String,
    },
    Resume,
    Retire {
        reason: String,
    },
}

/// Owner-only. Promotion and resume need a step-up; the approval is built
/// from the authenticated owner, never from the request body (INV-11).
async fn strategy_event(
    State(state): State<ApiState>,
    caller: Caller,
    Path(id): Path<StrategyVersionId>,
    Json(body): Json<StrategyEventRequest>,
) -> Result<Json<Value>, ApiError> {
    caller.require_owner()?;
    let now = state.clock.now();
    let event = match body {
        StrategyEventRequest::StartResearch => StageEvent::StartResearch,
        StrategyEventRequest::RejectResearch { reason } => StageEvent::RejectResearch { reason },
        StrategyEventRequest::PassResearch { evidence } => StageEvent::PassResearch { evidence },
        StrategyEventRequest::Promote { to, evidence } => {
            caller.require_step_up(now)?;
            StageEvent::Promote {
                to,
                approval: OwnerApproval {
                    approved_by: caller.user.id,
                    approved_at: now,
                    evidence,
                },
            }
        }
        StrategyEventRequest::Suspend { reason } => StageEvent::Suspend {
            by: caller.user.id,
            reason,
        },
        StrategyEventRequest::Resume => {
            caller.require_step_up(now)?;
            StageEvent::Resume { by: caller.user.id }
        }
        StrategyEventRequest::Retire { reason } => StageEvent::Retire { reason },
    };
    let stage = state
        .registry
        .transition(id, &event, &caller.actor())
        .await
        .map_err(registry_error)?;
    Ok(Json(json!({ "stage": stage })))
}

// ---------- instruments, bars, backtests ----------

async fn instruments(
    State(state): State<ApiState>,
    _caller: Caller,
) -> Result<Json<Value>, ApiError> {
    let today = state.clock.now().date_naive();
    let specs = state.market.instruments(today).await.map_err(internal)?;
    Ok(Json(serde_json::to_value(specs).map_err(internal)?))
}

#[derive(Deserialize)]
struct BarRange {
    from: NaiveDate,
    to: NaiveDate,
}

async fn bars(
    State(state): State<ApiState>,
    _caller: Caller,
    Path(id): Path<InstrumentId>,
    Query(range): Query<BarRange>,
) -> Result<Json<Value>, ApiError> {
    if range.from > range.to || (range.to - range.from).num_days() > 3700 {
        return Err(ApiError::BadRequest("invalid date range".to_owned()));
    }
    let bars = state
        .market
        .daily_bars(id, range.from, range.to, state.clock.now())
        .await
        .map_err(internal)?;
    Ok(Json(serde_json::to_value(bars).map_err(internal)?))
}

fn data(state: &ApiState) -> Result<&std::sync::Arc<dyn qd_app::ports::DataAdmin>, ApiError> {
    state
        .data
        .as_ref()
        .ok_or_else(|| ApiError::Conflict("data management is not available".to_owned()))
}

#[derive(Deserialize)]
struct NewInstrument {
    toml: String,
}

async fn add_instrument(
    State(state): State<ApiState>,
    caller: Caller,
    Json(body): Json<NewInstrument>,
) -> Result<Json<Value>, ApiError> {
    caller.require_owner()?;
    data(&state)?
        .add_instrument(&body.toml, &caller.actor())
        .await
        .map(Json)
        .map_err(settings_error)
}

#[derive(Deserialize)]
struct BarUpload {
    csv: String,
    #[serde(default)]
    accept_jumps: bool,
}

async fn import_bars(
    State(state): State<ApiState>,
    caller: Caller,
    Path(id): Path<InstrumentId>,
    Json(body): Json<BarUpload>,
) -> Result<Json<Value>, ApiError> {
    caller.require_owner()?;
    data(&state)?
        .import_bars(id, &body.csv, body.accept_jumps, &caller.actor())
        .await
        .map(Json)
        .map_err(settings_error)
}

async fn bar_quality(
    State(state): State<ApiState>,
    _caller: Caller,
    Path(id): Path<InstrumentId>,
) -> Result<Json<Value>, ApiError> {
    data(&state)?
        .quality(id)
        .await
        .map(Json)
        .map_err(|e| ApiError::BadRequest(e.0))
}

/// The strategy implementations in this build (logic version, name, parameters).
async fn strategy_catalog(_caller: Caller) -> Result<Json<Value>, ApiError> {
    let entries = qd_strategy::catalog::catalog().map_err(internal)?;
    Ok(Json(Value::Array(
        entries
            .iter()
            .map(|c| {
                json!({
                    "logic_version": c.strategy.logic_version(),
                    "name": c.strategy.name(),
                    "parameters": c.parameters,
                })
            })
            .collect(),
    )))
}

// ---------- validation and evidence ----------

async fn run_validation(
    State(state): State<ApiState>,
    caller: Caller,
    Json(request): Json<qd_app::ports::ValidationRequest>,
) -> Result<Json<Value>, ApiError> {
    caller.require_owner()?;
    state
        .validator
        .validate(&request, &caller.actor())
        .await
        .map(Json)
        .map_err(|e| ApiError::BadRequest(e.0))
}

#[derive(Deserialize)]
struct EvidenceQuery {
    version: Option<StrategyVersionId>,
}

/// A record without its per-trade detail, for lists.
fn evidence_summary(r: &qd_app::evidence::EvidenceRecord) -> Value {
    let pick = |key: &str| r.report.get(key).cloned().unwrap_or(Value::Null);
    json!({
        "id": r.id,
        "version": r.version,
        "kind": r.kind,
        "passed": r.passed,
        "created_at": r.created_at,
        "from": pick("from"),
        "to": pick("to"),
        "instruments": pick("instruments"),
        "checks": pick("checks"),
        "oos": pick("oos"),
        "holdout": pick("holdout"),
        "monte_carlo": pick("monte_carlo"),
        "evidence": pick("evidence"),
    })
}

async fn evidence_list(
    State(state): State<ApiState>,
    _caller: Caller,
    Query(q): Query<EvidenceQuery>,
) -> Result<Json<Vec<Value>>, ApiError> {
    let records = state.evidence.list(q.version).await.map_err(internal)?;
    Ok(Json(records.iter().map(evidence_summary).collect()))
}

async fn evidence_one(
    State(state): State<ApiState>,
    _caller: Caller,
    Path(id): Path<EvidenceId>,
) -> Result<Json<qd_app::evidence::EvidenceRecord>, ApiError> {
    state
        .evidence
        .get(id)
        .await
        .map_err(internal)?
        .map(Json)
        .ok_or(ApiError::NotFound)
}

// ---------- advisory AI (INV-04) ----------

fn ai(state: &ApiState) -> Result<&std::sync::Arc<dyn qd_app::ports::AiAdvisory>, ApiError> {
    state
        .ai
        .as_ref()
        .ok_or_else(|| ApiError::Conflict("advisory AI is disabled".to_owned()))
}

async fn ai_run(State(state): State<ApiState>, caller: Caller) -> Result<Json<Value>, ApiError> {
    caller.require_owner()?;
    let service = ai(&state)?;
    state
        .audit
        .record(&caller.actor(), "ai.run", json!({}))
        .await
        .map_err(internal)?;
    service
        .run()
        .await
        .map(Json)
        .map_err(|e| ApiError::Conflict(e.0))
}

#[derive(Deserialize)]
struct AdviceQuery {
    decision: Option<DecisionId>,
}

async fn ai_advice(
    State(state): State<ApiState>,
    _caller: Caller,
    Query(q): Query<AdviceQuery>,
) -> Result<Json<Value>, ApiError> {
    ai(&state)?
        .advice(q.decision)
        .await
        .map(Json)
        .map_err(internal)
}

async fn ai_scorecard(
    State(state): State<ApiState>,
    _caller: Caller,
) -> Result<Json<Value>, ApiError> {
    ai(&state)?.scorecard().await.map(Json).map_err(internal)
}

async fn review(State(state): State<ApiState>, _caller: Caller) -> Result<Json<Value>, ApiError> {
    state.reviewer.review().await.map(Json).map_err(internal)
}

async fn record_review(
    State(state): State<ApiState>,
    caller: Caller,
    Path(version): Path<StrategyVersionId>,
) -> Result<Json<Value>, ApiError> {
    caller.require_owner()?;
    state
        .reviewer
        .record(version, &caller.actor())
        .await
        .map(Json)
        .map_err(|e| ApiError::BadRequest(e.0))
}

// ---------- settings and secrets (ADR 0013) ----------

fn settings_error(e: qd_app::ports::SettingsError) -> ApiError {
    match e {
        qd_app::ports::SettingsError::Invalid(m) => ApiError::BadRequest(m),
        qd_app::ports::SettingsError::Store(e) => internal(e),
    }
}

async fn settings_view(
    State(state): State<ApiState>,
    _caller: Caller,
) -> Result<Json<Value>, ApiError> {
    state
        .settings_admin
        .view()
        .await
        .map(Json)
        .map_err(internal)
}

#[derive(Deserialize)]
struct SettingsBody {
    values: serde_json::Map<String, Value>,
}

async fn settings_update(
    State(state): State<ApiState>,
    caller: Caller,
    Path(section): Path<String>,
    Json(body): Json<SettingsBody>,
) -> Result<Json<Value>, ApiError> {
    caller.require_step_up(state.clock.now())?;
    state
        .settings_admin
        .update(&section, &body.values, &caller.actor())
        .await
        .map(Json)
        .map_err(settings_error)
}

async fn settings_reset(
    State(state): State<ApiState>,
    caller: Caller,
    Path(section): Path<String>,
) -> Result<Json<Value>, ApiError> {
    caller.require_step_up(state.clock.now())?;
    state
        .settings_admin
        .reset(&section, &caller.actor())
        .await
        .map(Json)
        .map_err(settings_error)
}

/// Every catalog secret with its status. Values are never returned (INV-15).
async fn settings_history(
    State(state): State<ApiState>,
    caller: Caller,
    Path(section): Path<String>,
) -> Result<Json<Value>, ApiError> {
    caller.require_owner()?;
    state
        .settings_admin
        .history(&section)
        .await
        .map(Json)
        .map_err(settings_error)
}

async fn settings_restore(
    State(state): State<ApiState>,
    caller: Caller,
    Path((section, version)): Path<(String, i64)>,
) -> Result<Json<Value>, ApiError> {
    caller.require_step_up(state.clock.now())?;
    state
        .settings_admin
        .restore(&section, version, &caller.actor())
        .await
        .map(Json)
        .map_err(settings_error)
}

/// Re-encrypts every stored secret under a new master key (owner, step-up).
async fn rotate_master_key(
    State(state): State<ApiState>,
    caller: Caller,
) -> Result<Json<Value>, ApiError> {
    caller.require_step_up(state.clock.now())?;
    state
        .settings_admin
        .rotate_master_key(&caller.actor())
        .await
        .map(Json)
        .map_err(|e| match e {
            qd_app::ports::SettingsError::Invalid(m) => ApiError::Conflict(m),
            qd_app::ports::SettingsError::Store(e) => internal(e),
        })
}

async fn secrets_status(
    State(state): State<ApiState>,
    _caller: Caller,
) -> Result<Json<Value>, ApiError> {
    let stored = state.secrets.status().await.map_err(internal)?;
    let secrets: Vec<Value> = qd_app::secrets::SECRETS
        .iter()
        .map(|spec| {
            let status = stored.iter().find(|s| s.name == spec.name);
            json!({
                "name": spec.name,
                "provider": spec.provider,
                "label": spec.label,
                "help": spec.help,
                "set": status.is_some(),
                "readable": status.is_none_or(|s| s.readable),
                "updated_at": status.and_then(|s| s.updated_at),
                "updated_by": status.and_then(|s| s.updated_by.clone()),
            })
        })
        .collect();
    Ok(Json(
        json!({ "available": state.secrets.available(), "secrets": secrets }),
    ))
}

#[derive(Deserialize)]
struct SecretBody {
    value: String,
}

async fn secret_set(
    State(state): State<ApiState>,
    caller: Caller,
    Path(name): Path<String>,
    Json(body): Json<SecretBody>,
) -> Result<Json<Value>, ApiError> {
    caller.require_step_up(state.clock.now())?;
    if qd_app::secrets::spec(&name).is_none() {
        return Err(ApiError::NotFound);
    }
    let value = qd_app::ports::SecretValue::new(body.value);
    state
        .secrets
        .set(&name, &value, &caller.actor())
        .await
        .map_err(|e| ApiError::BadRequest(e.0))?;
    // Only the status comes back, never the value.
    Ok(Json(json!({ "name": name, "set": true })))
}

async fn secret_clear(
    State(state): State<ApiState>,
    caller: Caller,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    caller.require_step_up(state.clock.now())?;
    if qd_app::secrets::spec(&name).is_none() {
        return Err(ApiError::NotFound);
    }
    state
        .secrets
        .clear(&name, &caller.actor())
        .await
        .map_err(|e| ApiError::BadRequest(e.0))?;
    Ok(Json(json!({ "name": name, "set": false })))
}

// ---------- paper trading ----------

fn paper(state: &ApiState) -> Result<&std::sync::Arc<dyn qd_app::ports::PaperTrading>, ApiError> {
    state
        .paper
        .as_ref()
        .ok_or_else(|| ApiError::Conflict("paper trading is not configured".to_owned()))
}

async fn paper_state(
    State(state): State<ApiState>,
    _caller: Caller,
) -> Result<Json<Value>, ApiError> {
    let runner = paper(&state)?;
    runner.state().await.map(Json).map_err(internal)
}

#[derive(Deserialize)]
struct PaperRunBody {
    through: NaiveDate,
}

async fn paper_run(
    State(state): State<ApiState>,
    caller: Caller,
    Json(body): Json<PaperRunBody>,
) -> Result<Json<Value>, ApiError> {
    caller.require_owner()?;
    if body.through > state.clock.now().date_naive() {
        return Err(ApiError::BadRequest(
            "through cannot be in the future".to_owned(),
        ));
    }
    let runner = paper(&state)?;
    state
        .audit
        .record(
            &caller.actor(),
            "paper.run",
            json!({ "through": body.through }),
        )
        .await
        .map_err(internal)?;
    // Paper runs only simulate; their failures (busy, restore refused) are
    // shown to the owner as conflicts rather than hidden as internal errors.
    runner
        .run_through(body.through)
        .await
        .map(Json)
        .map_err(|e| ApiError::Conflict(e.0))
}

async fn backtest(
    State(state): State<ApiState>,
    caller: Caller,
    Json(request): Json<BacktestRequest>,
) -> Result<Json<Value>, ApiError> {
    caller.require_owner()?;
    let report = state
        .backtests
        .run(&request)
        .await
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    state
        .audit
        .record(
            &caller.actor(),
            "backtest.run",
            serde_json::to_value(&request).map_err(internal)?,
        )
        .await
        .map_err(internal)?;
    Ok(Json(report))
}

// ---------- live arming ----------

#[derive(Deserialize)]
struct LiveArmed {
    armed: bool,
}

/// Arming needs the owner and a recent step-up (INV-14). Disarming needs only
/// the owner: it reduces risk.
async fn live_armed(
    State(state): State<ApiState>,
    caller: Caller,
    Json(body): Json<LiveArmed>,
) -> Result<Json<Value>, ApiError> {
    caller.require_owner()?;
    if body.armed {
        caller.require_step_up(state.clock.now())?;
    }
    // With a live account configured, that is the account armed (ADR 0014).
    let account = state
        .settings
        .live_account_id
        .unwrap_or(state.settings.account_id);
    state
        .accounts
        .set_live_armed(account, body.armed, &caller.actor())
        .await
        .map_err(|e| ApiError::Conflict(e.to_string()))?;
    Ok(Json(json!({ "armed": body.armed })))
}

// ---------- Zerodha Kite and live trading (ADR 0014) ----------

fn broker(state: &ApiState) -> Result<&std::sync::Arc<dyn qd_app::ports::BrokerLink>, ApiError> {
    state
        .broker
        .as_ref()
        .ok_or_else(|| ApiError::Conflict("the Zerodha connection is not available".to_owned()))
}

async fn kite_status(
    State(state): State<ApiState>,
    _caller: Caller,
) -> Result<Json<Value>, ApiError> {
    broker(&state)?.status().await.map(Json).map_err(internal)
}

/// Starts "Login with Zerodha": the owner is sent to Zerodha with a
/// one-time state that the callback checks.
async fn kite_login(
    State(state): State<ApiState>,
    caller: Caller,
) -> Result<Json<Value>, ApiError> {
    caller.require_owner()?;
    let url = broker(&state)?
        .login_url(&caller.actor())
        .await
        .map_err(|e| ApiError::Conflict(e.0))?;
    Ok(Json(json!({ "url": url })))
}

#[derive(Deserialize)]
struct KiteCallback {
    request_token: Option<String>,
    state: Option<String>,
    status: Option<String>,
}

/// Zerodha redirects here after a login. The session cookie is `SameSite=
/// Strict`, so it does not come with this cross-site redirect: the one-time
/// state proves the login was started by the owner.
async fn kite_callback(
    State(state): State<ApiState>,
    Query(query): Query<KiteCallback>,
) -> Response {
    let result = match (&query.request_token, &query.state, query.status.as_deref()) {
        (Some(token), Some(nonce), Some("success") | None) => match broker(&state) {
            Ok(b) => b.complete_login(nonce, token).await.map_err(|e| e.0),
            Err(e) => Err(e.to_string()),
        },
        _ => Err("Zerodha did not complete the login".to_owned()),
    };
    let target = match result {
        Ok(_) => "/#/broker?login=ok".to_owned(),
        Err(reason) => {
            tracing::warn!(reason = %reason, "Zerodha login failed");
            "/#/broker?login=failed".to_owned()
        }
    };
    (
        axum::http::StatusCode::SEE_OTHER,
        [(header::LOCATION, target)],
    )
        .into_response()
}

async fn kite_sync_bars(
    State(state): State<ApiState>,
    caller: Caller,
) -> Result<Json<Value>, ApiError> {
    caller.require_owner()?;
    let b = broker(&state)?;
    state
        .audit
        .record(&caller.actor(), "kite.sync_bars", json!({}))
        .await
        .map_err(internal)?;
    b.sync_bars()
        .await
        .map(Json)
        .map_err(|e| ApiError::Conflict(e.0))
}

/// Applies fills and places protection: risk-reducing only, so no step-up.
async fn kite_sync_fills(
    State(state): State<ApiState>,
    caller: Caller,
) -> Result<Json<Value>, ApiError> {
    caller.require_owner()?;
    let b = broker(&state)?;
    state
        .audit
        .record(&caller.actor(), "kite.sync_fills", json!({}))
        .await
        .map_err(internal)?;
    b.sync_fills()
        .await
        .map(Json)
        .map_err(|e| ApiError::Conflict(e.0))
}

fn live(state: &ApiState) -> Result<&std::sync::Arc<dyn qd_app::ports::PaperTrading>, ApiError> {
    state
        .live
        .as_ref()
        .ok_or_else(|| ApiError::Conflict("live trading is not configured".to_owned()))
}

async fn live_state(
    State(state): State<ApiState>,
    _caller: Caller,
) -> Result<Json<Value>, ApiError> {
    live(&state)?.state().await.map(Json).map_err(internal)
}

/// A live daily cycle can place real orders: owner and step-up (INV-14's
/// other conditions are checked again in the Order Gateway).
async fn live_run(
    State(state): State<ApiState>,
    caller: Caller,
    Json(body): Json<PaperRunBody>,
) -> Result<Json<Value>, ApiError> {
    caller.require_owner()?;
    caller.require_step_up(state.clock.now())?;
    if body.through > state.clock.now().date_naive() {
        return Err(ApiError::BadRequest(
            "through cannot be in the future".to_owned(),
        ));
    }
    let runner = live(&state)?;
    state
        .audit
        .record(
            &caller.actor(),
            "live.run",
            json!({ "through": body.through }),
        )
        .await
        .map_err(internal)?;
    runner
        .run_through(body.through)
        .await
        .map(Json)
        .map_err(|e| ApiError::Conflict(e.0))
}

#[derive(Deserialize)]
struct BookQuery {
    book: String,
}

async fn portfolio_view(
    State(state): State<ApiState>,
    _caller: Caller,
    Query(query): Query<BookQuery>,
) -> Result<Json<Value>, ApiError> {
    if !matches!(query.book.as_str(), "paper" | "live") {
        return Err(ApiError::BadRequest(
            "book must be paper or live".to_owned(),
        ));
    }
    state
        .portfolio
        .as_ref()
        .ok_or_else(|| ApiError::Conflict("portfolio view is not available".to_owned()))?
        .view(&query.book)
        .await
        .map(Json)
        .map_err(|e| ApiError::Conflict(e.0))
}

async fn notifications_test(
    State(state): State<ApiState>,
    caller: Caller,
) -> Result<Json<Value>, ApiError> {
    caller.require_owner()?;
    let notifier = state
        .notifier
        .as_ref()
        .ok_or_else(|| ApiError::Conflict("notifications are not available".to_owned()))?;
    notifier
        .notify("QuantDesk test message: notifications work.")
        .await
        .map_err(|e| ApiError::Conflict(e.0))?;
    Ok(Json(json!({ "sent": true })))
}

async fn openapi() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "application/yaml")], OPENAPI)
}
