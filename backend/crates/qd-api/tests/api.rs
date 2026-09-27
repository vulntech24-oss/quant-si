//! The API end to end against a real PostgreSQL (`DATABASE_URL`).

// Test code: a failed unwrap is a failed test. The workspace-wide unwrap/expect
// ban targets runtime code, and clippy's allow-unwrap-in-tests does not reach
// helper functions in integration-test files.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chrono::{TimeZone, Utc};
use qd_api::{ApiSettings, ApiState, auth, router};
use qd_app::journal::{DecisionRecord, JournalEntry};
use qd_app::live::Environment;
use qd_app::ports::{
    AuthStore, BacktestRequest, BacktestRunner, Clock, Journal, Role, StoreError, UserRecord,
};
use qd_app::registry::StrategyRegistry;
use qd_domain::ids::{AccountId, DecisionId, InstrumentId, StrategyId, StrategyVersionId, UserId};
use qd_domain::num::Ratio;
use qd_domain::outcome::{DecisionOutcome, NoTradeReason, RiskLimitBreach};
use qd_domain::proposal::{AccountMode, StrategyRef};
use qd_store::{AccountRecord, Stores};
use sqlx::PgPool;
use tower::ServiceExt;

struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 3, 16, 10, 0, 0).unwrap()
    }
}

struct FakeBacktests;

#[async_trait::async_trait]
impl BacktestRunner for FakeBacktests {
    async fn run(&self, _: &BacktestRequest) -> Result<serde_json::Value, StoreError> {
        Ok(serde_json::json!({ "metrics": { "trades": 0 } }))
    }
}

/// Validation runs are tested end to end in qd-server; the API only routes them.
struct FakeValidator;

#[async_trait::async_trait]
impl qd_app::ports::Validator for FakeValidator {
    async fn validate(
        &self,
        _: &qd_app::ports::ValidationRequest,
        _: &str,
    ) -> Result<serde_json::Value, StoreError> {
        Err(StoreError("no data".to_owned()))
    }
}

/// Records what the API asked of it; the real settings admin is tested in qd-server.
#[derive(Default)]
struct FakeSettingsAdmin {
    updates: std::sync::Mutex<Vec<(String, String)>>,
}

#[async_trait::async_trait]
impl qd_app::ports::SettingsAdmin for FakeSettingsAdmin {
    async fn view(&self) -> Result<serde_json::Value, StoreError> {
        Ok(serde_json::json!({ "sections": [] }))
    }
    async fn update(
        &self,
        section: &str,
        _: &serde_json::Map<String, serde_json::Value>,
        actor: &str,
    ) -> Result<serde_json::Value, qd_app::ports::SettingsError> {
        if section == "bogus" {
            return Err(qd_app::ports::SettingsError::Invalid(
                "unknown settings section bogus".to_owned(),
            ));
        }
        self.updates
            .lock()
            .unwrap()
            .push((section.to_owned(), actor.to_owned()));
        Ok(serde_json::json!({ "name": section }))
    }
    async fn reset(
        &self,
        section: &str,
        _: &str,
    ) -> Result<serde_json::Value, qd_app::ports::SettingsError> {
        Ok(serde_json::json!({ "name": section }))
    }
}

/// Accepts only the login state "good-state"; the real link is tested in qd-server.
struct FakeBrokerLink;

#[async_trait::async_trait]
impl qd_app::ports::BrokerLink for FakeBrokerLink {
    async fn status(&self) -> Result<serde_json::Value, StoreError> {
        Ok(serde_json::json!({ "enabled": true }))
    }
    async fn login_url(&self, _: &str) -> Result<String, StoreError> {
        Ok("https://kite.zerodha.com/connect/login?v=3&api_key=k".to_owned())
    }
    async fn complete_login(&self, state: &str, _: &str) -> Result<serde_json::Value, StoreError> {
        if state == "good-state" {
            Ok(serde_json::json!({ "user_id": "AB1234" }))
        } else {
            Err(StoreError("unknown or expired login state".to_owned()))
        }
    }
    async fn sync_bars(&self) -> Result<serde_json::Value, StoreError> {
        Ok(serde_json::json!({ "instruments": [] }))
    }
    async fn sync_fills(&self) -> Result<serde_json::Value, StoreError> {
        Ok(serde_json::json!({}))
    }
}

/// Records messages.
#[derive(Default)]
struct FakeNotifier(std::sync::Mutex<Vec<String>>);

#[async_trait::async_trait]
impl qd_app::ports::Notifier for FakeNotifier {
    async fn notify(&self, text: &str) -> Result<(), StoreError> {
        self.0.lock().unwrap().push(text.to_owned());
        Ok(())
    }
}

const OWNER_PASSWORD: &str = "correct horse battery staple";
const VIEWER_PASSWORD: &str = "viewer password 123";

struct App {
    router: Router,
    stores: Stores,
    account: AccountId,
    notifier: Arc<FakeNotifier>,
}

async fn app(pool: PgPool) -> App {
    let stores = Stores::new(&pool);
    let now = FixedClock.now();
    for (name, role, password) in [
        ("owner", Role::Owner, OWNER_PASSWORD),
        ("viewer", Role::Viewer, VIEWER_PASSWORD),
    ] {
        stores
            .auth
            .create_user(&UserRecord {
                id: UserId::new_at(now),
                username: name.to_owned(),
                role,
                password_hash: auth::hash_password(password).unwrap(),
            })
            .await
            .unwrap();
    }
    let account = AccountId::new_at(now);
    stores
        .accounts
        .create(
            &AccountRecord {
                id: account,
                name: "paper".to_owned(),
                mode: AccountMode::Paper,
                currency: "INR".to_owned(),
                live_armed: false,
            },
            "test",
        )
        .await
        .unwrap();
    let notifier = Arc::new(FakeNotifier::default());
    let state = ApiState {
        monitor: qd_app::monitor::MonitorSettings::default(),
        data: None,
        portfolio: None,
        totp: Some(Arc::new(qd_store::settings::PgSecrets::new(
            pool.clone(),
            Some(qd_store::settings::MasterKey::new([7_u8; 32])),
            stores.audit.clone(),
        ))),
        live: None,
        broker: Some(Arc::new(FakeBrokerLink)),
        notifier: Some(notifier.clone()),
        auth: stores.auth.clone(),
        journal: stores.journal.clone(),
        halts: stores.halts.clone(),
        registry: StrategyRegistry::new(
            stores.registry.clone(),
            stores.audit.clone(),
            stores.evidence.clone(),
        ),
        market: stores.market.clone(),
        accounts: stores.accounts.clone(),
        audit: stores.audit.clone(),
        backtests: Arc::new(FakeBacktests),
        paper: None,
        validator: Arc::new(FakeValidator),
        ai: None,
        reviewer: Arc::new(qd_app::review::JournalReviewer {
            reader: stores.journal.clone(),
            evidence: stores.evidence.clone(),
            audit: stores.audit.clone(),
            clock: Arc::new(FixedClock),
            account: qd_domain::ids::AccountId::new_at(FixedClock.now()),
            criteria: toml::from_str(include_str!("../../../config/review.toml")).unwrap(),
        }),
        evidence: stores.evidence.clone(),
        settings_admin: Arc::new(FakeSettingsAdmin::default()),
        secrets: Arc::new(qd_store::settings::PgSecrets::new(
            pool.clone(),
            Some(qd_store::settings::MasterKey::new([7_u8; 32])),
            stores.audit.clone(),
        )),
        clock: Arc::new(FixedClock),
        settings: ApiSettings {
            account_id: account,
            environment: Environment::Development,
            live_trading_enabled: false,
            live_orders_compiled: false,
            secure_cookies: true,
            session_hours: 12,
            live_account_id: None,
        },
        limiter: Arc::new(auth::LoginLimiter::default()),
    };
    App {
        router: router(state),
        stores,
        account,
        notifier,
    }
}

async fn call(
    app: &App,
    method: &str,
    uri: &str,
    cookie: Option<&str>,
    body: Option<serde_json::Value>,
    csrf: bool,
) -> (StatusCode, axum::http::HeaderMap, serde_json::Value) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(c) = cookie {
        req = req.header(header::COOKIE, c);
    }
    if csrf {
        req = req.header(auth::CSRF_HEADER, auth::CSRF_VALUE);
    }
    let req = match body {
        Some(b) => req
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(b.to_string())),
        None => req.body(Body::empty()),
    }
    .unwrap();
    let res = app.router.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, headers, json)
}

async fn login(app: &App, user: &str, password: &str) -> String {
    let (status, headers, _) = call(
        app,
        "POST",
        "/api/auth/login",
        None,
        Some(serde_json::json!({"username": user, "password": password})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let cookie = headers
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    assert!(
        cookie.contains("HttpOnly")
            && cookie.contains("SameSite=Strict")
            && cookie.contains("Secure")
    );
    cookie.split(';').next().unwrap().to_owned()
}

#[sqlx::test(migrator = "qd_store::MIGRATOR")]
async fn login_sessions_and_security_headers(pool: PgPool) {
    let app = app(pool.clone()).await;
    let (status, headers, _) = call(&app, "GET", "/api/status", None, None, false).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(headers.get("x-content-type-options").unwrap(), "nosniff");
    assert!(
        headers
            .get("content-security-policy")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("frame-ancestors 'none'")
    );

    let cookie = login(&app, "owner", OWNER_PASSWORD).await;
    let (status, _, me) = call(&app, "GET", "/api/auth/me", Some(&cookie), None, false).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["role"], "owner");
    // The raw token is never stored, only its hash.
    let token = cookie.split('=').nth(1).unwrap();
    let raw: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions WHERE token_hash = $1")
        .bind(token)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(raw, 0);

    let (status, _, _) = call(&app, "POST", "/api/auth/logout", Some(&cookie), None, true).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = call(&app, "GET", "/api/auth/me", Some(&cookie), None, false).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrator = "qd_store::MIGRATOR")]
async fn failed_logins_are_throttled(pool: PgPool) {
    let app = app(pool).await;
    for _ in 0..5 {
        let (status, _, _) = call(
            &app,
            "POST",
            "/api/auth/login",
            None,
            Some(serde_json::json!({"username": "owner", "password": "wrong password!!"})),
            true,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    let (status, _, _) = call(
        &app,
        "POST",
        "/api/auth/login",
        None,
        Some(serde_json::json!({"username": "owner", "password": OWNER_PASSWORD})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
}

#[sqlx::test(migrator = "qd_store::MIGRATOR")]
async fn mutations_need_the_csrf_header_and_the_owner(pool: PgPool) {
    let app = app(pool).await;
    let owner = login(&app, "owner", OWNER_PASSWORD).await;
    let (status, _, _) = call(
        &app,
        "POST",
        "/api/halts",
        Some(&owner),
        Some(serde_json::json!({"reason": "x"})),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let viewer = login(&app, "viewer", VIEWER_PASSWORD).await;
    let (status, _, _) = call(
        &app,
        "POST",
        "/api/halts",
        Some(&viewer),
        Some(serde_json::json!({"reason": "x"})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = call(&app, "GET", "/api/halts", Some(&viewer), None, false).await;
    assert_eq!(status, StatusCode::OK);
}

#[sqlx::test(migrator = "qd_store::MIGRATOR")]
async fn invariant_07_rearming_a_halt_needs_a_step_up(pool: PgPool) {
    let app = app(pool).await;
    let owner = login(&app, "owner", OWNER_PASSWORD).await;
    let (status, _, halt) = call(
        &app,
        "POST",
        "/api/halts",
        Some(&owner),
        Some(serde_json::json!({"reason": "pause"})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, _, status_json) = call(&app, "GET", "/api/status", Some(&owner), None, false).await;
    assert_eq!(status_json["entries_halted"], true);

    let rearm = format!("/api/halts/{}/rearm", halt["id"].as_str().unwrap());
    let (status, _, body) = call(&app, "POST", &rearm, Some(&owner), None, true).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "step_up_required");
    let (status, _, _) = call(
        &app,
        "POST",
        "/api/auth/step-up",
        Some(&owner),
        Some(serde_json::json!({"password": "wrong password!!"})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = call(
        &app,
        "POST",
        "/api/auth/step-up",
        Some(&owner),
        Some(serde_json::json!({"password": OWNER_PASSWORD})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, cleared) = call(&app, "POST", &rearm, Some(&owner), None, true).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cleared["active"], false);
}

#[sqlx::test(migrator = "qd_store::MIGRATOR")]
async fn invariant_14_arming_needs_step_up_and_a_live_account(pool: PgPool) {
    let app = app(pool).await;
    let owner = login(&app, "owner", OWNER_PASSWORD).await;
    let (status, _, body) = call(
        &app,
        "POST",
        "/api/account/live-armed",
        Some(&owner),
        Some(serde_json::json!({"armed": true})),
        true,
    )
    .await;
    assert_eq!(
        (status, body["code"].clone()),
        (StatusCode::FORBIDDEN, serde_json::json!("step_up_required"))
    );
    call(
        &app,
        "POST",
        "/api/auth/step-up",
        Some(&owner),
        Some(serde_json::json!({"password": OWNER_PASSWORD})),
        true,
    )
    .await;
    // The account is a paper account: the database refuses to arm it.
    let (status, _, _) = call(
        &app,
        "POST",
        "/api/account/live-armed",
        Some(&owner),
        Some(serde_json::json!({"armed": true})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let account = qd_app::ports::AccountStore::account(app.stores.accounts.as_ref(), app.account)
        .await
        .unwrap()
        .unwrap();
    assert!(!account.live_armed);
}

#[sqlx::test(migrator = "qd_store::MIGRATOR")]
async fn invariant_17_a_risk_blocked_decision_is_shown_as_no_trade_with_its_reason(pool: PgPool) {
    let app = app(pool).await;
    let now = FixedClock.now();
    let record = DecisionRecord {
        id: DecisionId::new_at(now),
        at: now,
        as_of_date: now.date_naive(),
        account: app.account,
        instrument: InstrumentId::new_at(now),
        strategy: StrategyRef {
            strategy_id: StrategyId::new_at(now),
            name: "Trend pullback".to_owned(),
            version_id: StrategyVersionId::new_at(now),
            version_number: 1,
            logic_version: "trend-pullback-1.0.0".to_owned(),
            git_sha: "abc".to_owned(),
        },
        regime: qd_strategy::regime::Regime::TrendUp,
        feature_set_version: "features-v1".to_owned(),
        outcome: DecisionOutcome::NoTrade {
            reason: NoTradeReason::RiskLimit(RiskLimitBreach::DailyLoss {
                limit: Ratio::new(rust_decimal::Decimal::new(2, 2)).unwrap(),
                current: Ratio::new(rust_decimal::Decimal::new(21, 3)).unwrap(),
            }),
        },
        proposal: None,
        approval: None,
    };
    app.stores
        .journal
        .append(&JournalEntry::Decision(Box::new(record.clone())))
        .await
        .unwrap();
    let viewer = login(&app, "viewer", VIEWER_PASSWORD).await;
    let (status, _, list) = call(&app, "GET", "/api/decisions", Some(&viewer), None, false).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list[0]["headline"], "NO TRADE");
    assert_eq!(list[0]["reason"]["code"], "risk_limit");
    assert_eq!(list[0]["reason"]["detail"]["breach"], "daily_loss");
    let (status, _, one) = call(
        &app,
        "GET",
        &format!("/api/decisions/{}", record.id),
        Some(&viewer),
        None,
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(one["summary"]["headline"], "NO TRADE");
}

#[sqlx::test(migrator = "qd_store::MIGRATOR")]
async fn strategy_promotion_needs_step_up_and_uses_the_authenticated_owner(pool: PgPool) {
    let app = app(pool.clone()).await;
    let now = FixedClock.now();
    let version = qd_app::registry::StrategyVersionRecord {
        reference: StrategyRef {
            strategy_id: StrategyId::new_at(now),
            name: "Trend pullback".to_owned(),
            version_id: StrategyVersionId::new_at(now),
            version_number: 1,
            logic_version: "trend-pullback-1.0.0".to_owned(),
            git_sha: "abc".to_owned(),
        },
        parameters: serde_json::json!({}),
        rr_floor: rust_decimal::Decimal::new(15, 1),
    };
    let registry = StrategyRegistry::new(
        app.stores.registry.clone(),
        app.stores.audit.clone(),
        app.stores.evidence.clone(),
    );
    registry.register(&version, "test").await.unwrap();
    let id = version.reference.version_id;
    let owner = login(&app, "owner", OWNER_PASSWORD).await;
    let events = format!("/api/strategies/{id}/events");
    let evidence = qd_domain::ids::EvidenceId::new_at(now);
    qd_app::evidence::EvidenceStore::record(
        app.stores.evidence.as_ref(),
        &qd_app::evidence::EvidenceRecord {
            id: evidence,
            version: id,
            kind: qd_app::evidence::EvidenceKind::Validation,
            passed: true,
            report: serde_json::json!({"evidence": []}),
            created_at: now,
        },
    )
    .await
    .unwrap();
    for e in [
        serde_json::json!({"event": "start_research"}),
        serde_json::json!({"event": "pass_research", "evidence": evidence}),
    ] {
        let (status, _, _) = call(&app, "POST", &events, Some(&owner), Some(e), true).await;
        assert_eq!(status, StatusCode::OK);
    }
    let promote = serde_json::json!({"event": "promote", "to": "paper", "evidence": evidence});
    let (status, _, _) = call(
        &app,
        "POST",
        &events,
        Some(&owner),
        Some(promote.clone()),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    call(
        &app,
        "POST",
        "/api/auth/step-up",
        Some(&owner),
        Some(serde_json::json!({"password": OWNER_PASSWORD})),
        true,
    )
    .await;
    let (status, _, body) = call(&app, "POST", &events, Some(&owner), Some(promote), true).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["stage"]["stage"], "paper");
    // Skipping a stage is a conflict.
    let skip = serde_json::json!({"event": "promote", "to": "full", "evidence": evidence});
    let (status, _, _) = call(&app, "POST", &events, Some(&owner), Some(skip), true).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let owner_id: String =
        sqlx::query_scalar("SELECT user_id::text FROM users WHERE username = 'owner'")
            .fetch_one(&pool)
            .await
            .unwrap();
    let approved_by: String = sqlx::query_scalar("SELECT event->'approval'->>'approved_by' FROM strategy_stage_events WHERE event->>'event' = 'promote'").fetch_one(&pool).await.unwrap();
    assert_eq!(approved_by, owner_id);
}

#[sqlx::test(migrator = "qd_store::MIGRATOR")]
async fn paper_endpoints_report_when_paper_trading_is_not_configured(pool: PgPool) {
    let app = app(pool).await;
    let owner = login(&app, "owner", OWNER_PASSWORD).await;
    let (status, _, body) = call(&app, "GET", "/api/paper", Some(&owner), None, false).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "conflict");
    let run = serde_json::json!({"through": "2026-03-16"});
    let (status, _, _) = call(
        &app,
        "POST",
        "/api/paper/run",
        Some(&owner),
        Some(run.clone()),
        false,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "the CSRF header is still required"
    );
    let (status, _, _) = call(
        &app,
        "POST",
        "/api/paper/run",
        Some(&owner),
        Some(run),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

async fn step_up(app: &App, cookie: &str) {
    let (status, _, _) = call(
        app,
        "POST",
        "/api/auth/step-up",
        Some(cookie),
        Some(serde_json::json!({"password": OWNER_PASSWORD})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[sqlx::test(migrator = "qd_store::MIGRATOR")]
async fn invariant_15_secrets_are_write_only_encrypted_and_audited_by_name(pool: PgPool) {
    let app = app(pool.clone()).await;
    let owner = login(&app, "owner", OWNER_PASSWORD).await;
    let viewer = login(&app, "viewer", VIEWER_PASSWORD).await;
    let secret = "sk-test-DO-NOT-LEAK-1234567890";
    let body = serde_json::json!({ "value": secret });

    // Viewers cannot write; the owner needs a fresh step-up.
    let (status, _, _) = call(
        &app,
        "PUT",
        "/api/secrets/openai_api_key",
        Some(&viewer),
        Some(body.clone()),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, b) = call(
        &app,
        "PUT",
        "/api/secrets/openai_api_key",
        Some(&owner),
        Some(body.clone()),
        true,
    )
    .await;
    assert_eq!(
        (status, b["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("step_up_required"))
    );
    step_up(&app, &owner).await;
    let (status, _, b) = call(
        &app,
        "PUT",
        "/api/secrets/openai_api_key",
        Some(&owner),
        Some(body),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!b.to_string().contains(secret));

    // Status says "set", and nowhere returns the value.
    let (status, _, list) = call(&app, "GET", "/api/secrets", Some(&viewer), None, false).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["available"], true);
    let entry = list["secrets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "openai_api_key")
        .unwrap()
        .clone();
    assert_eq!(
        (entry["set"].as_bool(), entry["readable"].as_bool()),
        (Some(true), Some(true))
    );
    assert!(!list.to_string().contains(secret));

    // At rest it is ciphertext; the audit log names the secret, never the value.
    let stored: Vec<u8> =
        sqlx::query_scalar("SELECT ciphertext FROM secrets WHERE name = 'openai_api_key'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!String::from_utf8_lossy(&stored).contains(secret));
    let audit: Vec<serde_json::Value> =
        sqlx::query_scalar("SELECT detail FROM audit_log WHERE action = 'secret.set'")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(audit.len(), 1);
    assert!(!audit[0].to_string().contains(secret));

    // Server-side adapters can read it; a different key cannot.
    let reader = qd_store::settings::PgSecrets::new(
        pool.clone(),
        Some(qd_store::settings::MasterKey::new([7_u8; 32])),
        app.stores.audit.clone(),
    );
    let value = qd_app::ports::SecretReader::get(&reader, "openai_api_key")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(value.expose(), secret);
    assert_eq!(format!("{value:?}"), "SecretValue(***)");
    let wrong = qd_store::settings::PgSecrets::new(
        pool.clone(),
        Some(qd_store::settings::MasterKey::new([8_u8; 32])),
        app.stores.audit.clone(),
    );
    assert!(
        qd_app::ports::SecretReader::get(&wrong, "openai_api_key")
            .await
            .is_err()
    );

    // Unknown names, pasted newlines, and clearing.
    let (status, _, _) = call(
        &app,
        "PUT",
        "/api/secrets/root_password",
        Some(&owner),
        Some(serde_json::json!({"value": "x"})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = call(
        &app,
        "PUT",
        "/api/secrets/kite_api_key",
        Some(&owner),
        Some(serde_json::json!({"value": "abc\n"})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, _) = call(
        &app,
        "DELETE",
        "/api/secrets/openai_api_key",
        Some(&owner),
        None,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let remaining: i64 = sqlx::query_scalar("SELECT count(*) FROM secrets")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(remaining, 0, "a cleared secret is really gone");
}

#[sqlx::test(migrator = "qd_store::MIGRATOR")]
async fn settings_changes_need_the_owner_and_a_step_up(pool: PgPool) {
    let app = app(pool).await;
    let owner = login(&app, "owner", OWNER_PASSWORD).await;
    let viewer = login(&app, "viewer", VIEWER_PASSWORD).await;
    let body = serde_json::json!({ "values": { "risk_per_trade": "0.005" } });
    let (status, _, _) = call(&app, "GET", "/api/settings", Some(&viewer), None, false).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = call(
        &app,
        "PUT",
        "/api/settings/risk",
        Some(&viewer),
        Some(body.clone()),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, b) = call(
        &app,
        "PUT",
        "/api/settings/risk",
        Some(&owner),
        Some(body.clone()),
        true,
    )
    .await;
    assert_eq!(
        (status, b["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("step_up_required"))
    );
    let (status, _, _) = call(
        &app,
        "PUT",
        "/api/settings/risk",
        Some(&owner),
        Some(body.clone()),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "the CSRF header is required");
    step_up(&app, &owner).await;
    let (status, _, _) = call(
        &app,
        "PUT",
        "/api/settings/risk",
        Some(&owner),
        Some(body.clone()),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = call(
        &app,
        "PUT",
        "/api/settings/bogus",
        Some(&owner),
        Some(body),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrator = "qd_store::MIGRATOR")]
async fn zerodha_login_needs_the_owner_and_the_callback_needs_the_one_time_state(pool: PgPool) {
    let app = app(pool).await;
    let owner = login(&app, "owner", OWNER_PASSWORD).await;
    let viewer = login(&app, "viewer", VIEWER_PASSWORD).await;
    let (status, _, _) = call(&app, "POST", "/api/kite/login", Some(&viewer), None, true).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = call(&app, "POST", "/api/kite/login", Some(&owner), None, false).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "CSRF header required");
    let (status, _, body) = call(&app, "POST", "/api/kite/login", Some(&owner), None, true).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body["url"]
            .as_str()
            .unwrap()
            .starts_with("https://kite.zerodha.com/")
    );

    // The redirect back from Zerodha carries no session cookie.
    for (query, outcome) in [
        (
            "request_token=abc&state=good-state&status=success",
            "login=ok",
        ),
        (
            "request_token=abc&state=forged&status=success",
            "login=failed",
        ),
        (
            "request_token=abc&state=good-state&status=cancelled",
            "login=failed",
        ),
        ("state=good-state", "login=failed"),
    ] {
        let (status, headers, _) = call(
            &app,
            "GET",
            &format!("/api/kite/callback?{query}"),
            None,
            None,
            false,
        )
        .await;
        assert_eq!(status, StatusCode::SEE_OTHER, "{query}");
        let location = headers.get(header::LOCATION).unwrap().to_str().unwrap();
        assert!(location.ends_with(outcome), "{query} -> {location}");
    }
}

#[sqlx::test(migrator = "qd_store::MIGRATOR")]
async fn invariant_14_a_live_run_needs_the_owner_and_a_step_up(pool: PgPool) {
    let app = app(pool).await;
    let owner = login(&app, "owner", OWNER_PASSWORD).await;
    let viewer = login(&app, "viewer", VIEWER_PASSWORD).await;
    let body = serde_json::json!({ "through": "2026-03-13" });
    let (status, _, _) = call(
        &app,
        "POST",
        "/api/live/run",
        Some(&viewer),
        Some(body.clone()),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, err) = call(
        &app,
        "POST",
        "/api/live/run",
        Some(&owner),
        Some(body),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(err["code"], "step_up_required");
}

#[sqlx::test(migrator = "qd_store::MIGRATOR")]
async fn the_test_notification_is_owner_only(pool: PgPool) {
    let app = app(pool).await;
    let owner = login(&app, "owner", OWNER_PASSWORD).await;
    let viewer = login(&app, "viewer", VIEWER_PASSWORD).await;
    let (status, _, _) = call(
        &app,
        "POST",
        "/api/notifications/test",
        Some(&viewer),
        None,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = call(
        &app,
        "POST",
        "/api/notifications/test",
        Some(&owner),
        None,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(app.notifier.0.lock().unwrap().len(), 1);
}

fn totp_code(secret: &str, step: i64) -> String {
    let key = qd_api::totp::base32_decode(secret).unwrap();
    format!(
        "{:06}",
        qd_api::totp::hotp(&key, u64::try_from(step).unwrap(), 6).unwrap()
    )
}

#[sqlx::test(migrator = "qd_store::MIGRATOR")]
async fn two_factor_login_needs_a_fresh_authenticator_code(pool: PgPool) {
    let app = app(pool).await;
    let owner = login(&app, "owner", OWNER_PASSWORD).await;
    let (status, _, err) = call(
        &app,
        "POST",
        "/api/auth/totp/setup",
        Some(&owner),
        None,
        true,
    )
    .await;
    assert_eq!(
        (status, err["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("step_up_required"))
    );
    let pw = serde_json::json!({ "password": OWNER_PASSWORD });
    call(
        &app,
        "POST",
        "/api/auth/step-up",
        Some(&owner),
        Some(pw),
        true,
    )
    .await;
    let (status, _, setup) = call(
        &app,
        "POST",
        "/api/auth/totp/setup",
        Some(&owner),
        None,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let secret = setup["secret"].as_str().unwrap().to_owned();
    assert!(
        setup["uri"]
            .as_str()
            .unwrap()
            .starts_with("otpauth://totp/QuantDesk:owner?secret=")
    );
    let step = qd_api::totp::step_at(FixedClock.now());

    // A wrong code does not enable it; the right one does.
    let wrong = serde_json::json!({ "code": "000000" });
    let (status, _, _) = call(
        &app,
        "POST",
        "/api/auth/totp/enable",
        Some(&owner),
        Some(wrong),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let right = serde_json::json!({ "code": totp_code(&secret, step) });
    let (status, _, _) = call(
        &app,
        "POST",
        "/api/auth/totp/enable",
        Some(&owner),
        Some(right),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // The password alone no longer logs in.
    let attempt = |code: Option<String>| {
        let mut body = serde_json::json!({ "username": "owner", "password": OWNER_PASSWORD });
        if let Some(c) = code {
            body["code"] = serde_json::json!(c);
        }
        body
    };
    let (status, _, err) = call(
        &app,
        "POST",
        "/api/auth/login",
        None,
        Some(attempt(None)),
        true,
    )
    .await;
    assert_eq!(
        (status, err["code"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("totp_required"))
    );
    // The code used for enabling cannot be replayed; the next one works once.
    let used = Some(totp_code(&secret, step));
    let (status, _, _) = call(
        &app,
        "POST",
        "/api/auth/login",
        None,
        Some(attempt(used)),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let next = Some(totp_code(&secret, step + 1));
    let (status, _, _) = call(
        &app,
        "POST",
        "/api/auth/login",
        None,
        Some(attempt(next.clone())),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = call(
        &app,
        "POST",
        "/api/auth/login",
        None,
        Some(attempt(next)),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "each code works once");
}

#[sqlx::test(migrator = "qd_store::MIGRATOR")]
async fn sessions_are_listed_without_tokens_and_can_be_logged_out_everywhere(pool: PgPool) {
    let app = app(pool).await;
    let first = login(&app, "owner", OWNER_PASSWORD).await;
    let second = login(&app, "owner", OWNER_PASSWORD).await;
    let (status, _, list) = call(
        &app,
        "GET",
        "/api/auth/sessions",
        Some(&second),
        None,
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let list = list.as_array().unwrap().clone();
    assert_eq!(list.len(), 2);
    assert_eq!(list.iter().filter(|s| s["current"] == true).count(), 1);
    let token = first.split('=').nth(1).unwrap();
    assert!(!serde_json::to_string(&list).unwrap().contains(token));

    // Revoke the other session by id.
    let other = list.iter().find(|s| s["current"] == false).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, _, _) = call(
        &app,
        "DELETE",
        &format!("/api/auth/sessions/{other}"),
        Some(&second),
        None,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = call(&app, "GET", "/api/status", Some(&first), None, false).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Log out everywhere, this session included.
    let third = login(&app, "owner", OWNER_PASSWORD).await;
    let (status, _, out) = call(
        &app,
        "POST",
        "/api/auth/sessions/revoke-all",
        Some(&second),
        None,
        true,
    )
    .await;
    assert_eq!((status, out["revoked"].as_u64()), (StatusCode::OK, Some(2)));
    for cookie in [&second, &third] {
        let (status, _, _) = call(&app, "GET", "/api/status", Some(cookie), None, false).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
}
