//! Server configuration, startup sequence and health routes.

// Test code: a failed unwrap is a failed test. The workspace-wide unwrap/expect
// ban targets runtime code, and clippy's allow-unwrap-in-tests does not reach
// helper functions in integration-test files.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{TimeZone, Utc};
use qd_app::ports::{BrokerPosition, HaltStore, Reconciler};
use qd_domain::halt::HaltKind;
use qd_domain::ids::InstrumentId;
use qd_server::config::{ConfigError, ServerConfig};
use qd_server::http::{HealthState, router};
use qd_server::startup::startup;
use qd_store::PgHaltStore;
use sqlx::PgPool;
use tower::ServiceExt;

fn example_config() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/quantdesk.example.toml")
}

fn env_with_db(key: &str) -> Option<String> {
    (key == "QD_DATABASE_URL").then(|| "postgres://user:hunter2@localhost/db".to_owned())
}

#[test]
fn the_example_config_loads_with_safe_defaults() {
    let config = ServerConfig::load(&example_config(), &env_with_db).unwrap();
    assert!(!config.live.live_trading_enabled);
    assert_eq!(
        config.live.environment,
        qd_app::live::Environment::Development
    );
    assert_eq!(config.file.bind, "127.0.0.1:8080");
    assert_eq!(config.costs.schedules().len(), 3);
}

#[test]
fn invariant_15_the_database_url_comes_from_the_environment_and_is_redacted() {
    assert!(matches!(
        ServerConfig::load(&example_config(), &|_| None),
        Err(ConfigError::MissingEnv("QD_DATABASE_URL"))
    ));
    let config = ServerConfig::load(&example_config(), &env_with_db).unwrap();
    let debug = format!("{config:?}");
    assert!(
        !debug.contains("hunter2"),
        "secret leaked into Debug output"
    );
}

#[test]
fn invariant_14_live_trading_outside_production_or_with_unverified_costs_refuses_to_start() {
    let mut config = ServerConfig::load(&example_config(), &env_with_db).unwrap();
    config.live.live_trading_enabled = true;
    assert!(matches!(config.validate(), Err(ConfigError::Unsafe(_))));
    config.live.environment = qd_app::live::Environment::Production;
    // With unverified schedules it refuses whatever the build.
    let mut unverified = config.cost_schedules.clone();
    for s in &mut unverified {
        s.verification.status = qd_domain::costs::VerificationStatus::Unverified;
    }
    let mut refused = config.clone();
    refused.costs = qd_domain::costs::ScheduleCostModel::new(qd_domain::costs::CostScheduleSet {
        schedules: unverified,
    })
    .unwrap();
    assert!(matches!(refused.validate(), Err(ConfigError::Unsafe(_))));
    // With the shipped (verified) schedules, only a live-orders build may start.
    assert_eq!(
        config.validate().is_ok(),
        qd_app::live::live_orders_compiled()
    );
}

/// A broker whose positions the (empty) book does not hold.
struct FakeBroker(Vec<BrokerPosition>);

#[async_trait::async_trait]
impl Reconciler for FakeBroker {
    async fn reconcile(&self) -> Result<Vec<String>, qd_app::ports::StoreError> {
        let book = qd_app::positions::reconcile_book(&[], &self.0);
        Ok(book.iter().map(|m| format!("{m:?}")).collect())
    }
}

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 3, 16, 3, 30, 0).unwrap()
}

#[sqlx::test(migrator = "qd_store::MIGRATOR")]
async fn invariant_07_without_a_broker_entries_stay_halted_after_startup(pool: PgPool) {
    let halts = PgHaltStore::new(pool.clone());
    let report = startup(&pool, &halts, None, now()).await.unwrap();
    assert!(!report.startup_halt_cleared);
    let active: Vec<_> = halts
        .load()
        .await
        .unwrap()
        .into_iter()
        .filter(|h| h.is_active_at(now()))
        .collect();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].kind(), HaltKind::Startup);
}

#[sqlx::test(migrator = "qd_store::MIGRATOR")]
async fn invariant_07_startup_clears_only_after_clean_reconciliation(pool: PgPool) {
    let halts = PgHaltStore::new(pool.clone());
    let unreconciled = FakeBroker(vec![BrokerPosition {
        instrument: InstrumentId::new_at(now()),
        net_quantity: rust_decimal::Decimal::ONE,
    }]);
    let report = startup(&pool, &halts, Some(&unreconciled), now())
        .await
        .unwrap();
    assert!(!report.startup_halt_cleared);

    let clean = FakeBroker(vec![]);
    let report = startup(&pool, &halts, Some(&clean), now()).await.unwrap();
    assert!(report.startup_halt_cleared);
    // The first boot's startup halt is still active: it was never reconciled.
    let active = halts
        .load()
        .await
        .unwrap()
        .into_iter()
        .filter(|h| h.is_active_at(now()))
        .count();
    assert_eq!(active, 1);
}

#[sqlx::test(migrator = "qd_store::MIGRATOR")]
async fn health_and_readiness_report_the_halt_state(pool: PgPool) {
    let halts = Arc::new(PgHaltStore::new(pool.clone()));
    startup(&pool, halts.as_ref(), None, now()).await.unwrap();
    let app = router(HealthState {
        pool,
        halts: halts.clone() as Arc<dyn HaltStore>,
        paper: None,
    });
    let health = app
        .clone()
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(health.status(), StatusCode::OK);
    let metrics = app
        .clone()
        .oneshot(Request::get("/metrics").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(metrics.status(), StatusCode::OK);
    let text = axum::body::to_bytes(metrics.into_body(), 1 << 20)
        .await
        .unwrap();
    let text = String::from_utf8(text.to_vec()).unwrap();
    assert!(text.contains("qd_up 1"), "{text}");
    assert!(
        text.contains("qd_entries_halted 1"),
        "startup keeps entries halted: {text}"
    );
    let ready = app
        .oneshot(Request::get("/ready").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(ready.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(ready.into_body(), 1 << 20)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["entries_halted"], true);
    assert_eq!(json["active_halts"][0]["kind"], "startup");
}
