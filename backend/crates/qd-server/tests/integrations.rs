//! "Login with Zerodha", Telegram notifications and their settings, against
//! a local fake server (never Zerodha or Telegram).

// Test code: a failed unwrap is a failed test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::Uri;
use qd_app::ports::{BrokerLink, Notifier, SecretReader, SecretStore, SecretValue, SettingsAdmin};
use qd_server::config::ServerConfig;
use qd_server::kite::{DynKite, DynLive};
use qd_server::notify::TelegramNotifier;
use qd_server::runtime::Runtime;
use qd_store::Stores;
use serde_json::{Map, Value, json};
use sqlx::PgPool;

fn example_config() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/quantdesk.example.toml")
}

type Seen = Arc<Mutex<Vec<(String, String)>>>;

async fn fake(State(seen): State<Seen>, uri: Uri, body: String) -> axum::Json<Value> {
    let path = uri.path().to_owned();
    seen.lock().unwrap().push((path.clone(), body.clone()));
    if path == "/session/token" {
        return axum::Json(json!({
            "status": "success",
            "data": {"user_id": "AB1234", "access_token": "tok1"},
        }));
    }
    if path == "/instruments/historical/7/day" {
        // Tue 22 Sep 2026 has no bar, and 24 Sep jumps 38%.
        return axum::Json(json!({"status": "success", "data": {"candles": [
            ["2026-09-18T00:00:00+0530", 100, 101, 99, 100, 10],
            ["2026-09-21T00:00:00+0530", 100, 102, 99, 101, 10],
            ["2026-09-23T00:00:00+0530", 101, 102, 100, 101.5, 10],
            ["2026-09-24T00:00:00+0530", 140, 141, 139, 140, 10],
            ["2026-09-25T00:00:00+0530", 140, 142, 139, 141, 10],
        ]}}));
    }
    if path.ends_with("/sendMessage") {
        let chat: Value = serde_json::from_str(&body).unwrap();
        if chat["chat_id"] == "999" {
            return axum::Json(json!({"ok": false, "description": "Bad Request: chat not found"}));
        }
        return axum::Json(json!({"ok": true, "result": {}}));
    }
    axum::Json(json!({"status": "error", "error_type": "InputException", "message": "no route"}))
}

async fn start() -> (String, Seen) {
    let seen: Seen = Arc::default();
    let app = axum::Router::new().fallback(fake).with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), seen)
}

async fn runtime(pool: &PgPool, fake_base: &str) -> Arc<Runtime> {
    let config = ServerConfig::load(&example_config(), &|k| {
        (k == "QD_DATABASE_URL").then(|| "postgres://unused".to_owned())
    })
    .unwrap();
    let stores = Stores::new(pool);
    let mut rt = Runtime::new(config, stores.clone(), Arc::new(qd_server::SystemClock));
    rt.secrets = Arc::new(qd_store::settings::PgSecrets::new(
        pool.clone(),
        Some(qd_store::settings::MasterKey::new([7_u8; 32])),
        stores.audit.clone(),
    ));
    rt.kite_base = fake_base.to_owned();
    Arc::new(rt)
}

fn values(pairs: &[(&str, Value)]) -> Map<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

async fn secret(rt: &Runtime, name: &str, value: &str) {
    rt.secrets
        .set(name, &SecretValue::new(value.to_owned()), "owner")
        .await
        .unwrap();
}

fn state_of(url: &str) -> String {
    let query = url.split_once('?').unwrap().1;
    let params: Vec<(String, String)> = url::form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect();
    let redirect = &params
        .iter()
        .find(|(k, _)| k == "redirect_params")
        .unwrap()
        .1;
    url::form_urlencoded::parse(redirect.as_bytes())
        .into_owned()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
}

#[sqlx::test(migrator = "qd_store::MIGRATOR")]
async fn a_zerodha_login_stores_todays_token_only_for_the_configured_client(pool: PgPool) {
    let (base, seen) = start().await;
    let rt = runtime(&pool, &base).await;
    let kite = DynKite::new(rt.clone(), Arc::new(DynLive(rt.clone())));

    // Off, or without keys, no login starts.
    assert!(kite.login_url("owner").await.is_err());
    rt.update(
        "kite",
        &values(&[("enabled", json!(true)), ("user_id", json!("ZZ9999"))]),
        "owner",
    )
    .await
    .unwrap();
    secret(&rt, "kite_api_key", "key1").await;
    assert!(
        kite.login_url("owner").await.is_err(),
        "needs the API secret"
    );
    secret(&rt, "kite_api_secret", "secret1").await;

    // A forged or reused state is refused before Zerodha is called.
    let url = kite.login_url("owner").await.unwrap();
    assert!(url.contains("api_key=key1"));
    assert!(kite.complete_login("forged", "rt1").await.is_err());
    assert!(seen.lock().unwrap().is_empty());

    // Logged in as someone else: the token is not stored.
    let err = kite
        .complete_login(&state_of(&url), "rt1")
        .await
        .unwrap_err();
    assert!(err.0.contains("AB1234"), "{err}");
    assert!(rt.secrets.get("kite_access_token").await.unwrap().is_none());
    let (path, body) = seen.lock().unwrap()[0].clone();
    assert_eq!(path, "/session/token");
    assert!(
        !body.contains("secret1"),
        "the secret is only used in the checksum"
    );

    rt.update("kite", &values(&[("user_id", json!("AB1234"))]), "owner")
        .await
        .unwrap();
    let url = kite.login_url("owner").await.unwrap();
    let state = state_of(&url);
    assert_eq!(
        kite.complete_login(&state, "rt1").await.unwrap(),
        json!({"user_id": "AB1234"})
    );
    let token = rt.secrets.get("kite_access_token").await.unwrap().unwrap();
    assert_eq!(token.expose(), "tok1");
    assert!(
        kite.complete_login(&state, "rt1").await.is_err(),
        "one use only"
    );
    let status = kite.status().await.unwrap();
    assert_eq!(status["access_token_set"], true);
    assert!(
        !status.to_string().contains("tok1"),
        "status never shows the token"
    );
}

#[sqlx::test(migrator = "qd_store::MIGRATOR")]
async fn kite_settings_are_validated(pool: PgPool) {
    let rt = runtime(&pool, "http://unused").await;
    for (field, value) in [
        ("market_protection", json!("0")),
        ("stop_limit_buffer", json!("0.5")),
        ("variety", json!("bracket")),
        ("user_id", json!("AB 12")),
    ] {
        assert!(
            rt.update("kite", &values(&[(field, value.clone())]), "owner")
                .await
                .is_err(),
            "{field} = {value}"
        );
    }
    // Enabling needs the client id.
    assert!(
        rt.update("kite", &values(&[("enabled", json!(true))]), "owner")
            .await
            .is_err()
    );
}

#[sqlx::test(migrator = "qd_store::MIGRATOR")]
async fn telegram_messages_go_to_the_configured_chat_when_switched_on(pool: PgPool) {
    let (base, seen) = start().await;
    let rt = runtime(&pool, &base).await;
    let notifier = TelegramNotifier::new(rt.clone(), &base).unwrap();
    assert!(notifier.notify("hi").await.is_err(), "no chat id yet");
    rt.update(
        "notifications",
        &values(&[("telegram_chat_id", json!("12345"))]),
        "owner",
    )
    .await
    .unwrap();
    assert!(notifier.notify("hi").await.is_err(), "no token yet");
    secret(&rt, "telegram_bot_token", "123:abc").await;

    // The test button sends even while notifications are off.
    notifier.notify("hello").await.unwrap();
    let (path, body) = seen.lock().unwrap().last().cloned().unwrap();
    assert_eq!(path, "/bot123:abc/sendMessage");
    let body: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body, json!({"chat_id": "12345", "text": "hello"}));

    // Alerts and summaries only when switched on.
    notifier.notify_if_enabled("summary").await;
    assert_eq!(seen.lock().unwrap().len(), 1);
    rt.update(
        "notifications",
        &values(&[("enabled", json!(true))]),
        "owner",
    )
    .await
    .unwrap();
    notifier.notify_if_enabled("summary").await;
    assert_eq!(seen.lock().unwrap().len(), 2);
    let warning = qd_app::monitor::Alert {
        severity: qd_app::monitor::Severity::Warning,
        code: "paper_stale",
        message: "stale".to_owned(),
    };
    notifier.alert(&warning).await;
    assert_eq!(seen.lock().unwrap().len(), 2, "warnings are below critical");

    // A refusal is reported without the token.
    rt.update(
        "notifications",
        &values(&[("telegram_chat_id", json!("999"))]),
        "owner",
    )
    .await
    .unwrap();
    let err = notifier.notify("x").await.unwrap_err();
    assert!(
        err.0.contains("chat not found") && !err.0.contains("123:abc"),
        "{err}"
    );
}

#[sqlx::test(migrator = "qd_store::MIGRATOR")]
async fn the_kite_import_reports_gaps_and_holds_back_suspect_bars(pool: PgPool) {
    let (base, _seen) = start().await;
    let rt = runtime(&pool, &base).await;
    let mut spec: qd_domain::instrument::InstrumentSpecData = toml::from_str(include_str!(
        "../../../config/instruments/examples/example-equity.toml"
    ))
    .unwrap();
    spec.broker_refs = vec![qd_domain::instrument::BrokerRef {
        broker: "kite".to_owned(),
        symbol: "EXAMPLE".to_owned(),
        token: Some("7".to_owned()),
    }];
    let spec = qd_domain::instrument::InstrumentSpec::new(spec).unwrap();
    rt.stores.market.add_instrument(&spec).await.unwrap();
    rt.update(
        "kite",
        &values(&[("enabled", json!(true)), ("user_id", json!("AB1234"))]),
        "owner",
    )
    .await
    .unwrap();
    secret(&rt, "kite_api_key", "key1").await;
    secret(&rt, "kite_access_token", "tok1").await;

    let report = qd_server::kite::sync_bars(&rt).await.unwrap();
    let row = &report["instruments"][0];
    assert_eq!(row["inserted"], 3, "{report}");
    assert_eq!(
        row["held_back"], 2,
        "the jump and every later bar wait for review"
    );
    let issues: Vec<&str> = row["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["issue"].as_str().unwrap())
        .collect();
    assert_eq!(issues, vec!["missing_day", "price_jump"]);
    assert_eq!(row["issues"][0]["date"], "2026-09-22");
}

#[sqlx::test(migrator = "qd_store::MIGRATOR")]
async fn instruments_and_bars_from_the_web_ui_are_validated_and_checked(pool: PgPool) {
    use qd_app::ports::DataAdmin;
    let rt = runtime(&pool, "http://unused").await;
    let data = qd_server::data::DataService(rt.clone());
    let spec_toml = include_str!("../../../config/instruments/examples/example-equity.toml");
    let spec = data.add_instrument(spec_toml, "owner").await.unwrap();
    let id: qd_domain::ids::InstrumentId = serde_json::from_value(spec["id"].clone()).unwrap();
    assert!(
        data.add_instrument(spec_toml, "owner").await.is_err(),
        "specs are immutable"
    );
    assert!(
        data.add_instrument("symbol = 1", "owner").await.is_err(),
        "invalid TOML is refused"
    );

    let jumpy = "date,open,high,low,close,volume\n\
                 2026-09-21,100,101,99,100,10\n\
                 2026-09-23,100,101,99,100,10\n\
                 2026-09-24,140,141,139,140,10\n";
    let err = data
        .import_bars(id, jumpy, false, "owner")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("price jump"), "{err}");
    assert!(
        data.import_bars(id, "date,open\n2026-09-21,1\n", false, "owner")
            .await
            .is_err()
    );
    let out = data.import_bars(id, jumpy, true, "owner").await.unwrap();
    assert_eq!(out["rows_written"], 3);
    let quality = data.quality(id).await.unwrap();
    let kinds: Vec<&str> = quality["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["issue"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, vec!["missing_day", "price_jump"]);
}

#[sqlx::test(migrator = "qd_store::MIGRATOR")]
async fn settings_history_restores_a_validated_earlier_version(pool: PgPool) {
    let rt = runtime(&pool, "http://unused").await;
    for v in ["0.10", "0.30"] {
        rt.update("data", &values(&[("max_close_jump", json!(v))]), "owner")
            .await
            .unwrap();
    }
    let history = rt.history("data").await.unwrap();
    let versions = history["versions"].as_array().unwrap();
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0]["version"], 2, "newest first");
    assert_eq!(versions[1]["value"]["max_close_jump"], "0.10");
    assert!(history["file_default"].is_object());

    rt.restore("data", 1, "owner").await.unwrap();
    assert_eq!(
        rt.effective()
            .await
            .unwrap()
            .data
            .max_close_jump
            .to_string(),
        "0.10"
    );
    // A restore is a new version; history is never rewritten.
    assert_eq!(
        rt.history("data").await.unwrap()["versions"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert!(rt.restore("data", 99, "owner").await.is_err());
}

#[sqlx::test(migrator = "qd_store::MIGRATOR")]
async fn a_master_key_rotation_reencrypts_everything_and_survives_a_crash(pool: PgPool) {
    use qd_app::ports::{AuthStore, SecretReader, TotpStore};
    let dir = std::env::temp_dir().join(format!("qd-keys-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("master.key");
    let old = qd_server::config::load_master_key(None, Some(&dir))
        .unwrap()
        .unwrap();
    let stores = Stores::new(&pool);
    let secrets =
        qd_store::settings::PgSecrets::new(pool.clone(), Some(old.clone()), stores.audit.clone());
    secrets
        .set(
            "openai_api_key",
            &SecretValue::new("sk-1".to_owned()),
            "owner",
        )
        .await
        .unwrap();
    let user = qd_domain::ids::UserId::new_at(chrono::Utc::now());
    stores
        .auth
        .create_user(&qd_app::ports::UserRecord {
            id: user,
            username: "owner".to_owned(),
            role: qd_app::ports::Role::Owner,
            password_hash: "x".to_owned(),
        })
        .await
        .unwrap();
    secrets
        .put_pending_totp(user, &SecretValue::new("GEZDGNBV".to_owned()))
        .await
        .unwrap();

    // Without a key file (QD_MASTER_KEY), rotation is refused.
    assert!(
        qd_server::keys::rotate(&secrets, None, "owner")
            .await
            .is_err()
    );

    let count = qd_server::keys::rotate(&secrets, Some(&file), "owner")
        .await
        .unwrap();
    assert_eq!(count, 2);
    assert!(dir.join("master.key.previous").exists());
    assert!(!dir.join("master.key.new").exists());
    assert_eq!(
        secrets
            .get("openai_api_key")
            .await
            .unwrap()
            .unwrap()
            .expose(),
        "sk-1"
    );
    let new = qd_server::config::load_master_key(None, Some(&dir))
        .unwrap()
        .unwrap();
    assert!(secrets.key_opens_all(&new).await.unwrap());
    assert!(!secrets.key_opens_all(&old).await.unwrap());

    // Simulate a crash after the database commit but before the file swap:
    // master.key holds the old key, master.key.new the one the data uses.
    std::fs::rename(&file, dir.join("master.key.new")).unwrap();
    std::fs::rename(dir.join("master.key.previous"), &file).unwrap();
    let restarted =
        qd_store::settings::PgSecrets::new(pool.clone(), Some(old), stores.audit.clone());
    let key = qd_server::keys::recover(&restarted, Some(&file))
        .await
        .unwrap()
        .unwrap();
    restarted.use_key(key);
    assert_eq!(
        restarted
            .get("openai_api_key")
            .await
            .unwrap()
            .unwrap()
            .expose(),
        "sk-1"
    );
    assert_eq!(
        restarted.totp(user).await.unwrap().unwrap().secret.expose(),
        "GEZDGNBV"
    );
    assert!(!dir.join("master.key.new").exists());
    let _ = std::fs::remove_dir_all(&dir);
}
