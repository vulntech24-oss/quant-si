//! Provider advisors against a local fake server (never a real provider).

// Test code: a failed unwrap is a failed test. The workspace-wide unwrap/expect
// ban targets runtime code, and clippy's allow-unwrap-in-tests does not reach
// helper functions in integration-test files.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, Uri};
use qd_ai::advisor::{Advisor, AdvisorError, AdvisorInput};
use qd_ai_providers::{Provider, ProviderAdvisor};
use qd_app::journal::AiStance;
use qd_domain::ids::DecisionId;
use rust_decimal::Decimal;
use serde_json::{Value, json};

#[derive(Default)]
struct Seen {
    path: String,
    auth: String,
    body: Value,
}

struct Fake {
    seen: Mutex<Seen>,
    reply: (StatusCode, Value),
}

async fn handler(
    State(fake): State<Arc<Fake>>,
    uri: Uri,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> (StatusCode, axum::Json<Value>) {
    let auth = ["authorization", "x-goog-api-key"]
        .iter()
        .find_map(|h| headers.get(*h).and_then(|v| v.to_str().ok()))
        .unwrap_or("")
        .to_owned();
    *fake.seen.lock().unwrap() = Seen {
        path: uri.path().to_owned(),
        auth,
        body,
    };
    (fake.reply.0, axum::Json(fake.reply.1.clone()))
}

async fn serve(status: StatusCode, reply: Value) -> (String, Arc<Fake>) {
    let fake = Arc::new(Fake {
        seen: Mutex::new(Seen::default()),
        reply: (status, reply),
    });
    let app = axum::Router::new()
        .fallback(handler)
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), fake)
}

fn input() -> AdvisorInput {
    AdvisorInput {
        decision: DecisionId::new_at(chrono::DateTime::UNIX_EPOCH),
        as_of_date: None,
        symbol: Some("TEST-EQ".to_owned()),
        outcome: "enter".to_owned(),
        reason_code: None,
        regime: Some("trend_up".to_owned()),
        setup_type: Some("pullback".to_owned()),
        entry: Some(Decimal::new(10050, 2)),
        stop: Some(Decimal::new(9800, 2)),
        target: Some(Decimal::new(10600, 2)),
        rr_net: Some(Decimal::new(21, 1)),
        risk_net: None,
        reward_net: None,
        costs_per_unit: None,
        ev_r: Some(Decimal::new(3, 1)),
        probabilities: None,
        evidence_count: Some(80),
        argument_against: Some("late in the trend".to_owned()),
    }
}

const ADVICE: &str =
    r#"{"stance":"caution","confidence":0.62,"summary":"Stop is tight.","flags":["tight stop"]}"#;

fn advisor(provider: Provider, base: &str) -> ProviderAdvisor {
    ProviderAdvisor::new(
        provider,
        "m-1",
        base,
        "sk-test-key".to_owned(),
        Duration::from_secs(5),
    )
    .unwrap()
}

#[tokio::test]
async fn openai_and_xai_use_the_responses_api_with_a_strict_schema() {
    for provider in [Provider::OpenAi, Provider::Xai] {
        let reply = json!({"output": [
            {"type": "reasoning"},
            {"type": "message", "content": [{"type": "output_text", "text": ADVICE}]},
        ]});
        let (base, fake) = serve(StatusCode::OK, reply).await;
        let a = advisor(provider, &base);
        assert_eq!(a.name(), format!("{}:m-1:v1", provider.code()));
        assert!(!format!("{a:?}").contains("sk-test-key"));
        let draft = a.advise(&input()).await.unwrap();
        assert_eq!(draft.stance, AiStance::Caution);
        assert_eq!(draft.confidence, Decimal::new(62, 2));
        assert_eq!(draft.flags, vec!["tight stop".to_owned()]);
        let seen = fake.seen.lock().unwrap();
        assert_eq!(seen.path, "/responses");
        assert_eq!(seen.auth, "Bearer sk-test-key");
        assert_eq!(seen.body["model"], "m-1");
        assert_eq!(seen.body["text"]["format"]["type"], "json_schema");
        assert_eq!(seen.body["text"]["format"]["strict"], true);
        let packet = seen.body["input"][1]["content"].as_str().unwrap();
        assert!(packet.contains("TEST-EQ"));
    }
}

#[tokio::test]
async fn gemini_uses_generate_content_with_a_json_schema() {
    let reply = json!({"candidates": [{"content": {"parts": [{"text": ADVICE}]}}]});
    let (base, fake) = serve(StatusCode::OK, reply).await;
    let draft = advisor(Provider::Gemini, &base)
        .advise(&input())
        .await
        .unwrap();
    assert_eq!(draft.summary, "Stop is tight.");
    let seen = fake.seen.lock().unwrap();
    assert_eq!(seen.path, "/models/m-1:generateContent");
    assert_eq!(seen.auth, "sk-test-key");
    assert_eq!(
        seen.body["generationConfig"]["responseMimeType"],
        "application/json"
    );
    assert!(seen.body["generationConfig"]["responseJsonSchema"].is_object());
}

#[tokio::test]
async fn refusals_errors_and_malformed_answers_produce_no_advice() {
    let refusal =
        json!({"output": [{"type": "message", "content": [{"type": "refusal", "refusal": "no"}]}]});
    let (base, _) = serve(StatusCode::OK, refusal).await;
    assert!(matches!(
        advisor(Provider::OpenAi, &base).advise(&input()).await,
        Err(AdvisorError::Invalid(_))
    ));

    let (base, _) = serve(
        StatusCode::UNAUTHORIZED,
        json!({"error": {"message": "invalid key"}}),
    )
    .await;
    let err = advisor(Provider::Xai, &base)
        .advise(&input())
        .await
        .unwrap_err();
    assert!(
        matches!(&err, AdvisorError::Failed(m) if m.contains("401") && !m.contains("sk-test-key"))
    );

    let bad = json!({"candidates": [{"content": {"parts": [{"text": r#"{"stance":"buy"}"#}]}}]});
    let (base, _) = serve(StatusCode::OK, bad).await;
    assert!(matches!(
        advisor(Provider::Gemini, &base).advise(&input()).await,
        Err(AdvisorError::Invalid(_))
    ));
}

#[test]
fn invariant_04_provider_advisors_cannot_reach_orders_limits_halts_or_credentials() {
    let manifest = include_str!("../Cargo.toml");
    let deps = manifest
        .split("[dependencies]")
        .nth(1)
        .unwrap()
        .split("[dev-dependencies]")
        .next()
        .unwrap();
    for forbidden in [
        "qd-broker",
        "qd-store",
        "qd-risk",
        "qd-backtest",
        "qd-server",
        "qd-api",
        "qd-strategy",
    ] {
        assert!(!deps.contains(forbidden), "depends on {forbidden}");
    }
    let source = include_str!("../src/lib.rs");
    for forbidden in [
        "OrderGateway",
        "PositionManager",
        "EntryAuthorization",
        "BrokerOrderExecutor",
        "HaltStore",
        "RiskConfig",
        "AccountStore",
        "StrategyRegistry",
        "SecretReader",
        "Journal",
    ] {
        assert!(!source.contains(forbidden), "lib.rs mentions {forbidden}");
    }
}
