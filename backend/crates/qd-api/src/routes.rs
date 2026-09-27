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
        .route("/status", get(status))
        .route("/decisions", get(decisions))
        .route("/decisions/{id}", get(decision))
        .route("/journal", get(journal))
        .route("/halts", get(halts).post(create_halt))
        .route("/halts/{id}/rearm", post(rearm_halt))
        .route("/strategies", get(strategies))
        .route("/strategies/{id}/events", post(strategy_event))
        .route("/instruments", get(instruments))
        .route("/instruments/{id}/bars", get(bars))
        .route("/backtests", post(backtest))
        .route("/validations", post(run_validation))
        .route("/evidence", get(evidence_list))
        .route("/evidence/{id}", get(evidence_one))
        .route("/ai/run", post(ai_run))
        .route("/ai/advice", get(ai_advice))
        .route("/ai/scorecard", get(ai_scorecard))
        .route("/review", get(review))
        .route("/review/{version}/record", post(record_review))
        .route("/paper", get(paper_state))
        .route("/paper/run", post(paper_run))
        .route("/account/live-armed", post(live_armed))
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
    // Unknown halt state counts as halted (INV-06).
    let entries_halted = !known || !active.is_empty();
    let health = qd_app::monitor::collect(
        state.halts.as_ref(),
        state.paper.as_deref(),
        now,
        qd_app::monitor::MonitorSettings::default(),
    )
    .await;
    Ok(Json(json!({
        "alerts": health.alerts,
        "paper": health.paper,
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
    service.run().await.map(Json).map_err(internal)
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
    state
        .accounts
        .set_live_armed(state.settings.account_id, body.armed, &caller.actor())
        .await
        .map_err(|e| ApiError::Conflict(e.to_string()))?;
    Ok(Json(json!({ "armed": body.armed })))
}

async fn openapi() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "application/yaml")], OPENAPI)
}
