//! The AI agent end to end in the server, against local fake servers only
//! (a scripted Responses API and a fake Zerodha): it adds an instrument from
//! the broker's list with its history, reads a quote and the technical
//! snapshot, and requests a paper trade, which the Risk Gate sizes below
//! the agent's allocation and the Order Gateway journals. Never a real
//! provider, never Zerodha, never a real order.

// Test code: a failed unwrap is a failed test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::Uri;
use chrono::{Duration, Utc};
use qd_app::ports::{AgentControl, SecretStore, SecretValue, SettingsAdmin};
use qd_server::agent::{DynAgent, KiteMarket};
use qd_server::config::ServerConfig;
use qd_server::runtime::Runtime;
use qd_store::{AccountRecord, Stores};
use serde_json::{Map, Value, json};
use sqlx::PgPool;

fn example_config() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/quantdesk.example.toml")
}

#[derive(Clone, Default)]
struct Fake {
    steps: Arc<AtomicUsize>,
    seen: Arc<Mutex<Vec<String>>>,
}

fn call(id: &str, name: &str, arguments: Value) -> Value {
    json!({"type": "function_call", "call_id": id, "name": name, "arguments": arguments.to_string()})
}

/// 320 daily bars, one per calendar day, ending yesterday (IST): closes 980 → 1299.
fn candles() -> Value {
    let ist = Utc::now() + Duration::minutes(330);
    let last = ist.date_naive() - Duration::days(1);
    let rows: Vec<Value> = (0..320_i64)
        .map(|n| {
            let date = last - Duration::days(319 - n);
            let close = 980 + n;
            json!([
                format!("{date}T00:00:00+0530"),
                close,
                close + 2,
                close - 2,
                close,
                1000
            ])
        })
        .collect();
    json!({"status": "success", "data": {"candles": rows}})
}

async fn fake(State(fake): State<Fake>, uri: Uri, body: String) -> axum::response::Response {
    use axum::response::IntoResponse;
    let path = uri.path().to_owned();
    fake.seen.lock().unwrap().push(path.clone());
    match path.as_str() {
        "/instruments/NSE" => "instrument_token,exchange_token,tradingsymbol,name,last_price,expiry,strike,tick_size,lot_size,instrument_type,segment,exchange\n\
            4242,17,ACME,ACME INDUSTRIES,0,,0,0.05,1,EQ,NSE,NSE\n\
            4343,18,ACME-FUT,ACME,0,2026-10-29,0,0.05,500,FUT,NFO-FUT,NFO\n"
            .into_response(),
        "/instruments/BSE" => "instrument_token,exchange_token,tradingsymbol,name,last_price,expiry,strike,tick_size,lot_size,instrument_type,segment,exchange\n".into_response(),
        "/instruments/historical/4242/day" => axum::Json(candles()).into_response(),
        "/quote/ohlc" => axum::Json(json!({"status": "success", "data": {"NSE:ACME": {
            "instrument_token": 4242, "last_price": 1301.5,
            "ohlc": {"open": 1299, "high": 1303, "low": 1298, "close": 1299}}}}))
        .into_response(),
        "/responses" => {
            let request: Value = serde_json::from_str(&body).unwrap();
            // Web research calls carry only the built-in search tool.
            if request["tools"][0]["type"] == "web_search" {
                return axum::Json(json!({"output": [{"type": "message", "content": [{
                    "type": "output_text", "text": "ACME won a large order.",
                    "annotations": [{"type": "url_citation", "url": "https://news.example/acme", "title": "ACME order"}]}]}]}))
                .into_response();
            }
            let n = fake.steps.fetch_add(1, Ordering::SeqCst);
            let output = match n {
                0 => json!([call("c1", "add_instrument", json!({"exchange": "NSE", "tradingsymbol": "ACME"}))]),
                1 => json!([
                    call("c2", "get_quotes", json!({"symbols": ["ACME"]})),
                    call("c3", "technical_snapshot", json!({"symbol": "ACME"})),
                    call("c4", "web_research", json!({"query": "ACME Industries news"})),
                ]),
                2 => json!([call("c5", "place_trade", json!({
                    "symbol": "ACME", "direction": "long", "entry_type": "limit",
                    "entry": 1299, "stop": 1260, "target": 1400, "max_holding_days": 15,
                    "allocation_inr": 100000, "p_target": 0.55, "p_stop": 0.35,
                    "thesis": "Order win; steady uptrend.",
                    "key_evidence": ["large order", "uptrend"],
                    "strongest_argument_against": "Extended above its averages.",
                    "sources": ["https://news.example/acme"],
                }))]),
                _ => json!([{"type": "message", "content": [{"type": "output_text", "text": "Requested one ACME trade."}]}]),
            };
            axum::Json(json!({"output": output})).into_response()
        }
        _ => axum::Json(json!({"status": "error", "error_type": "InputException", "message": "no route"})).into_response(),
    }
}

async fn start() -> (String, Fake) {
    let state = Fake::default();
    let app = axum::Router::new().fallback(fake).with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), state)
}

fn values(pairs: &[(&str, Value)]) -> Map<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

#[sqlx::test(migrator = "qd_store::MIGRATOR")]
async fn the_agent_adds_an_instrument_and_requests_a_paper_trade_the_risk_gate_sizes(pool: PgPool) {
    let (base, fake) = start().await;
    let config = ServerConfig::load(&example_config(), &|k| {
        (k == "QD_DATABASE_URL").then(|| "postgres://unused".to_owned())
    })
    .unwrap();
    let account = config.file.account_id;
    let stores = Stores::new(&pool);
    let mut rt = Runtime::new(config, stores.clone(), Arc::new(qd_server::SystemClock));
    rt.secrets = Arc::new(qd_store::settings::PgSecrets::new(
        pool.clone(),
        Some(qd_store::settings::MasterKey::new([7_u8; 32])),
        stores.audit.clone(),
    ));
    rt.kite_base.clone_from(&base);
    rt.agent_base = Some(base.clone());
    let rt = Arc::new(rt);
    stores
        .accounts
        .create(
            &AccountRecord {
                id: account,
                name: "paper".to_owned(),
                mode: qd_domain::proposal::AccountMode::Paper,
                currency: "INR".to_owned(),
                live_armed: false,
            },
            "test",
        )
        .await
        .unwrap();
    for (name, value) in [
        ("kite_api_key", "key1"),
        ("kite_access_token", "tok1"),
        ("openai_api_key", "sk-test"),
    ] {
        rt.secrets
            .set(name, &SecretValue::new(value.to_owned()), "owner")
            .await
            .unwrap();
    }
    let start_date = (Utc::now() - Duration::days(5)).date_naive();
    rt.update(
        "paper",
        &values(&[("start_date", json!(start_date))]),
        "owner",
    )
    .await
    .unwrap();
    rt.update(
        "kite",
        &values(&[("enabled", json!(true)), ("user_id", json!("AB1234"))]),
        "owner",
    )
    .await
    .unwrap();
    let agent = DynAgent {
        runtime: rt.clone(),
        market: Arc::new(KiteMarket::new(rt.clone())),
        portfolio: None,
    };

    // Off by default: no run, nothing sent.
    assert!(agent.run("research", None).await.is_err());
    assert_eq!(fake.steps.load(Ordering::SeqCst), 0);
    rt.update(
        "agent",
        &values(&[("enabled", json!(true)), ("model", json!("m1"))]),
        "owner",
    )
    .await
    .unwrap();
    let status = agent.status().await.unwrap();
    assert_eq!(status["book"], "paper");
    assert_eq!(status["key_set"], true);

    let record = agent.run("research", None).await.unwrap();
    assert_eq!(record["status"], "completed", "{record:#}");
    assert_eq!(record["summary"], "Requested one ACME trade.");
    let steps = record["steps"].as_array().unwrap();
    let tools: Vec<&str> = steps.iter().map(|s| s["tool"].as_str().unwrap()).collect();
    assert_eq!(
        tools,
        vec![
            "add_instrument",
            "get_quotes",
            "technical_snapshot",
            "web_research",
            "place_trade"
        ]
    );
    assert_eq!(
        steps[0]["result"]["import"]["inserted"], 320,
        "{:#}",
        steps[0]
    );
    assert_eq!(
        steps[1]["result"]["quotes"]["NSE:ACME"]["last_price"], 1301.5,
        "{:#}",
        steps[1]
    );
    assert_eq!(
        steps[3]["result"]["sources"][0]["url"],
        "https://news.example/acme"
    );

    // ₹1,00,000 at 1299 is 76 shares; the risk budget alone would allow more.
    let execution = &steps[4]["result"]["execution"];
    assert_eq!(execution["book"], "paper", "{:#}", steps[4]);
    assert_eq!(execution["requested_quantity"], "76");
    assert_eq!(execution["approved_quantity"], "76", "{execution:#}");
    assert!(execution["position"].is_string());

    let decision: Value = sqlx::query_scalar(
        "SELECT entry FROM journal WHERE kind = 'decision' ORDER BY seq DESC LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(decision["strategy"]["name"], "AI agent");
    assert_eq!(
        decision["proposal"]["probabilities"]["source"],
        "ai:openai:m1"
    );
    assert_eq!(decision["approval"]["capped_by_request"], "76");
    let intents: i64 =
        sqlx::query_scalar("SELECT count(*) FROM journal WHERE kind = 'order_intent'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(intents, 1);
    let predictions: i64 =
        sqlx::query_scalar("SELECT count(*) FROM journal WHERE kind = 'ai_prediction'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(predictions, 1);
    let runs = agent.runs(5).await.unwrap();
    assert_eq!(runs[0]["run_kind"], "research");
    let audited: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE action = 'instrument.add' AND actor = 'ai-agent'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(audited, 1);
    // The broker was only read: no order endpoint was called.
    assert!(
        fake.seen
            .lock()
            .unwrap()
            .iter()
            .all(|p| !p.starts_with("/orders") && !p.starts_with("/gtt"))
    );
}
