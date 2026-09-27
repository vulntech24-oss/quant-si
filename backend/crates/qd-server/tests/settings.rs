//! Settings saved from the web UI (ADR 0013): layering over the files,
//! validation, audit, reset, effect without restart, and the master key.

// Test code: a failed unwrap is a failed test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use qd_app::ports::{Reconciler, SettingsAdmin, SettingsError};
use qd_server::config::{ServerConfig, load_master_key};
use qd_server::runtime::{DynPaper, Runtime, flatten};
use qd_store::Stores;
use rust_decimal_macros::dec;
use serde_json::{Map, Value, json};
use sqlx::PgPool;

fn example_config() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/quantdesk.example.toml")
}

fn runtime(pool: &PgPool) -> Arc<Runtime> {
    let config = ServerConfig::load(&example_config(), &|k| {
        (k == "QD_DATABASE_URL").then(|| "postgres://unused".to_owned())
    })
    .unwrap();
    Arc::new(Runtime::new(
        config,
        Stores::new(pool),
        Arc::new(qd_server::SystemClock),
    ))
}

fn values(pairs: &[(&str, Value)]) -> Map<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

fn section<'a>(view: &'a Value, name: &str) -> &'a Value {
    view["sections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == name)
        .unwrap()
}

#[sqlx::test(migrator = "qd_store::MIGRATOR")]
async fn saved_settings_override_the_files_at_once_and_are_audited(pool: PgPool) {
    let rt = runtime(&pool);
    let before = rt.effective().await.unwrap();
    assert_eq!(
        before.risk.risk_per_trade.value(),
        rt.config.risk.risk_per_trade.value()
    );
    let view = rt.view().await.unwrap();
    assert_eq!(section(&view, "risk")["source"]["kind"], "file");
    assert!(
        view.to_string().contains("live_trading_enabled"),
        "shown read-only"
    );
    assert!(
        !flatten(&section(&view, "risk")["fields"]).is_empty(),
        "fields are listed"
    );

    rt.update(
        "risk",
        &values(&[("risk_per_trade", json!("0.004"))]),
        "user:owner",
    )
    .await
    .unwrap();
    // The next read sees it: no restart.
    let after = rt.effective().await.unwrap();
    assert_eq!(after.risk.risk_per_trade.value(), dec!(0.004));
    let view = rt.view().await.unwrap();
    let saved_rows: Vec<(String, Option<Value>)> =
        sqlx::query_as("SELECT section, value FROM settings_versions")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        section(&view, "risk")["source"]["kind"],
        "saved",
        "{saved_rows:?} {}",
        section(&view, "risk")["source"]
    );
    let audit: Value =
        sqlx::query_scalar("SELECT detail FROM audit_log WHERE action = 'settings.update'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(audit["changes"][0]["field"], "risk_per_trade");
    assert_eq!(audit["changes"][0]["to"], "0.004");

    // Reset goes back to the file value; history stays.
    rt.reset("risk", "user:owner").await.unwrap();
    assert_eq!(
        rt.effective().await.unwrap().risk.risk_per_trade.value(),
        rt.config.risk.risk_per_trade.value()
    );
    let versions: i64 = sqlx::query_scalar("SELECT count(*) FROM settings_versions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(versions, 2);
}

#[sqlx::test(migrator = "qd_store::MIGRATOR")]
async fn invalid_changes_are_refused_and_nothing_is_stored(pool: PgPool) {
    let rt = runtime(&pool);
    let refused = [
        ("risk", "stage_multipliers.paper", json!("1")), // paper must never size live (INV-14)
        ("risk", "risk_per_trade", json!("not a number")),
        ("risk", "no_such_field", json!("1")),
        ("ai", "timeout_seconds", json!("0")),
        ("ai", "enabled", json!("yes")), // wrong type
        ("paper", "initial_equity", json!("-5")),
        ("validation", "holdout_fraction", json!("1.5")),
        ("review", "max_brier", json!("2")),
    ];
    for (name, key, value) in refused {
        let result = rt
            .update(name, &values(&[(key, value.clone())]), "user:owner")
            .await;
        assert!(
            matches!(result, Err(SettingsError::Invalid(_))),
            "{name}.{key} = {value}: {result:?}"
        );
    }
    assert!(matches!(
        rt.update(
            "live",
            &values(&[("live_trading_enabled", json!(true))]),
            "user:owner"
        )
        .await,
        Err(SettingsError::Invalid(_))
    ));
    let versions: i64 = sqlx::query_scalar("SELECT count(*) FROM settings_versions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(versions, 0);
    // History is append-only (INV-16).
    rt.update(
        "ai",
        &values(&[("max_calls_per_day", json!("50"))]),
        "user:owner",
    )
    .await
    .unwrap();
    assert!(
        sqlx::query("UPDATE settings_versions SET value = NULL")
            .execute(&pool)
            .await
            .is_err()
    );
}

#[sqlx::test(migrator = "qd_store::MIGRATOR")]
async fn paper_trading_switches_off_and_on_from_the_ui(pool: PgPool) {
    let rt = runtime(&pool);
    let paper = DynPaper(rt.clone());
    // The example config has paper on; the account does not exist yet.
    assert_eq!(
        paper.reconcile().await.unwrap(),
        vec!["the configured account does not exist"]
    );
    rt.update("paper", &values(&[("enabled", json!(false))]), "user:owner")
        .await
        .unwrap();
    let problems = paper.reconcile().await.unwrap();
    assert!(problems[0].contains("paper trading is off"), "{problems:?}");
    assert_eq!(
        qd_app::ports::PaperTrading::state(&paper).await.unwrap(),
        json!({ "configured": false })
    );
    // Schedule: set, then clear with an empty value.
    rt.update(
        "paper",
        &values(&[
            ("enabled", json!(true)),
            ("daily_run_utc", json!("12:30:00")),
        ]),
        "user:owner",
    )
    .await
    .unwrap();
    assert_eq!(rt.daily_run_utc().await.unwrap().to_string(), "12:30:00");
    rt.update(
        "paper",
        &values(&[("daily_run_utc", json!(""))]),
        "user:owner",
    )
    .await
    .unwrap();
    assert!(rt.daily_run_utc().await.is_none());
}

#[test]
fn the_master_key_comes_from_the_environment_or_a_private_generated_file() {
    let hex = "11".repeat(32);
    assert!(load_master_key(Some(hex), None).unwrap().is_some());
    assert!(load_master_key(Some("zz".to_owned()), None).is_err());
    assert!(
        load_master_key(None, None).unwrap().is_none(),
        "no key: secrets disabled"
    );

    let dir = std::env::temp_dir().join(format!("qd-key-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let first = load_master_key(None, Some(&dir)).unwrap().unwrap();
    let path = dir.join("master.key");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    // Reused, not regenerated.
    let text = std::fs::read_to_string(&path).unwrap();
    let second = load_master_key(None, Some(&dir)).unwrap().unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    assert_eq!(format!("{first:?}"), "MasterKey(***)");
    assert_eq!(format!("{second:?}"), "MasterKey(***)");
    std::fs::remove_dir_all(&dir).unwrap();
}
