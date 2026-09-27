//! The Kite adapter and the live runner against a local fake Kite server
//! (never Zerodha): login, INV-14, INV-11 demotion, INV-06 reconciliation
//! halts and, with the `live-orders` feature, the order and GTT forms.
//! Synthetic test data, not market data.

// Test code: a failed unwrap is a failed test. The workspace-wide unwrap/expect
// ban targets runtime code, and clippy's allow-unwrap-in-tests does not reach
// helper functions in integration-test files.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::http::{HeaderMap, Method, Uri};
use chrono::{DateTime, Days, NaiveDate, NaiveTime, TimeZone, Utc};
use qd_app::evidence::{EvidenceKind, EvidenceRecord, EvidenceStore, FixedEvidence};
use qd_app::live::LivePolicy;
use qd_app::ports::{Clock, Evidence, EvidenceSource, HaltStore};
use qd_app::registry::{StrategyRegistry, StrategyVersionRecord};
use qd_broker_kite::broker::{OrderSettings, VarietyChoice};
use qd_broker_kite::client::KiteClient;
use qd_broker_kite::runner::{LiveDeps, LiveRunner, LiveSettings};
use qd_domain::costs::{CostScheduleSet, ScheduleCostModel};
use qd_domain::economics::OutcomeProbabilities;
use qd_domain::halt::{Halt, HaltKind, HaltScope};
use qd_domain::ids::{
    AccountId, EvidenceId, HaltId, InstrumentId, StrategyId, StrategyVersionId, UserId,
};
use qd_domain::instrument::{
    AssetClass, BrokerRef, CalendarId, Capabilities, CorrelationBucket, InstrumentKind,
    InstrumentSpec, InstrumentSpecData, OrderType, ProductType, ProtectionMode, Validity, Venue,
};
use qd_domain::lifecycle::strategy::{OwnerApproval, StageEvent, StrategyStage, TradingStage};
use qd_domain::market::{Bar, BarData};
use qd_domain::num::Currency;
use qd_domain::proposal::{AccountMode, StrategyRef};
use qd_risk::config::{RiskConfig, RiskConfigData};
use qd_store::{AccountRecord, Stores};
use qd_strategy::trend_pullback::TrendPullback;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde_json::{Value, json};
use sqlx::PgPool;

const COSTS: &str = include_str!("../../../config/costs/india-zerodha.toml");
const RISK: &str = include_str!("../../../config/risk.toml");

// ---------------------------------------------------------------- fake Kite

#[derive(Default)]
struct FakeKiteState {
    /// (method, path, body or query, authorization header).
    requests: Vec<(String, String, String, String)>,
    orders: Vec<Value>,
    holdings: Vec<Value>,
    last_price: Option<Decimal>,
    gtt_status: String,
}

type Shared = Arc<Mutex<FakeKiteState>>;

fn form(body: &str) -> std::collections::HashMap<String, String> {
    url::form_urlencoded::parse(body.as_bytes())
        .into_owned()
        .collect()
}

async fn fake_kite(
    State(state): State<Shared>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: String,
) -> axum::Json<Value> {
    let path = uri.path().to_owned();
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let version = headers.get("x-kite-version").and_then(|v| v.to_str().ok());
    assert_eq!(
        version,
        Some("3"),
        "every request carries X-Kite-Version: 3"
    );
    let mut s = state.lock().unwrap();
    let detail = if method == Method::GET {
        uri.query().unwrap_or("").to_owned()
    } else {
        body.clone()
    };
    s.requests
        .push((method.to_string(), path.clone(), detail, auth.clone()));
    let ok = |data: Value| axum::Json(json!({"status": "success", "data": data}));
    if path != "/session/token" && auth != "token key1:tok1" {
        return axum::Json(
            json!({"status": "error", "message": "bad token", "error_type": "TokenException"}),
        );
    }
    match (method.as_str(), path.as_str()) {
        ("POST", "/session/token") => ok(json!({"user_id": "AB1234", "access_token": "tok1"})),
        ("GET", "/orders") => ok(Value::Array(s.orders.clone())),
        ("POST", p) if p.starts_with("/orders/") => ok(json!({"order_id": "O1"})),
        ("GET", "/portfolio/holdings") => ok(Value::Array(s.holdings.clone())),
        ("GET", "/portfolio/positions") => ok(json!({"net": [], "day": []})),
        ("GET", "/quote/ltp") => {
            let price = s.last_price.unwrap_or(dec!(100));
            ok(json!({"NSE:TESTEQ": {"instrument_token": 1, "last_price": price}}))
        }
        ("POST", "/gtt/triggers") => ok(json!({"trigger_id": 9})),
        ("GET", "/gtt/triggers/9") => ok(json!({
            "id": 9,
            "status": s.gtt_status,
            "orders": [{"result": null}, {"result": null}],
        })),
        ("GET", p) if p.starts_with("/instruments/historical/") => ok(json!({"candles": [
            ["2026-09-24T00:00:00+0530", 100.5, 101, 99.5, 100.25, 1000],
        ]})),
        _ => axum::Json(
            json!({"status": "error", "message": "no route", "error_type": "InputException"}),
        ),
    }
}

async fn start_fake_kite() -> (String, Shared) {
    let state: Shared = Arc::new(Mutex::new(FakeKiteState {
        gtt_status: "active".to_owned(),
        ..FakeKiteState::default()
    }));
    let app = axum::Router::new()
        .fallback(fake_kite)
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), state)
}

fn client(base: &str, token: Option<&str>) -> KiteClient {
    KiteClient::new(
        base,
        "key1",
        token.map(str::to_owned),
        Duration::from_secs(5),
    )
    .unwrap()
}

fn requests(state: &Shared, method: &str, prefix: &str) -> Vec<(String, String)> {
    state
        .lock()
        .unwrap()
        .requests
        .iter()
        .filter(|(m, p, _, _)| m == method && p.starts_with(prefix))
        .map(|(_, p, d, _)| (p.clone(), d.clone()))
        .collect()
}

const SETTINGS: OrderSettings = OrderSettings {
    market_protection: -1,
    stop_limit_buffer: dec!(0.01),
    variety: VarietyChoice::Auto,
};

// ------------------------------------------------------------- the world

/// After all the synthetic bars; 17:30 IST, so orders go out as AMO.
struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2027, 9, 1, 12, 0, 0).unwrap()
    }
}

struct FakeEvidence;

impl EvidenceSource for FakeEvidence {
    fn evidence(&self, _: StrategyVersionId, _: &str) -> Option<Evidence> {
        Some(Evidence {
            probabilities: OutcomeProbabilities::new(
                dec!(0.50),
                dec!(0.30),
                dec!(0.20),
                "test-evidence",
                100,
            )
            .unwrap(),
            time_exit_r: Decimal::ZERO,
        })
    }
}

fn day(n: u64) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 1, 1).unwrap() + Days::new(n)
}

fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap()
}

fn spec() -> InstrumentSpec {
    InstrumentSpec::new(InstrumentSpecData {
        id: InstrumentId::new_at(t0()),
        version: 1,
        effective_from: NaiveDate::from_ymd_opt(2020, 1, 1).unwrap(),
        effective_to: None,
        symbol: "TEST-EQ".to_owned(),
        venue: Venue::Nse,
        asset_class: AssetClass::Equity,
        kind: InstrumentKind::CashEquity,
        underlying: None,
        currency: Currency::INR,
        tick_size: dec!(0.05),
        lot_size: Decimal::ONE,
        multiplier: Decimal::ONE,
        quantity_step: Decimal::ONE,
        min_quantity: Decimal::ONE,
        expiry: None,
        calendar_id: CalendarId("nse".to_owned()),
        correlation_bucket: CorrelationBucket("india_equity".to_owned()),
        broker_refs: vec![BrokerRef {
            broker: "kite".to_owned(),
            symbol: "TESTEQ".to_owned(),
            token: Some("1".to_owned()),
        }],
        capabilities: Capabilities {
            can_short_overnight: false,
            supports_market_orders: true,
            requires_market_protection: true,
            protection_modes: vec![ProtectionMode::BrokerOco],
            products: vec![ProductType::Delivery],
            order_types: vec![
                OrderType::Limit,
                OrderType::StopLimit,
                OrderType::StopMarket,
                OrderType::Market,
            ],
            validities: vec![Validity::Day, Validity::GoodTillCancelled],
        },
    })
    .unwrap()
}

/// An uptrend with a 20-bar triangle-wave pullback cycle.
fn trending_bars(len: u64) -> Vec<Bar> {
    (0..len)
        .map(|n| {
            let phase = Decimal::from(n % 20);
            let wave = if phase < dec!(10) {
                phase
            } else {
                dec!(20) - phase
            };
            let close = dec!(100) + dec!(0.3) * Decimal::from(n) - dec!(0.3) * wave;
            Bar::new(BarData {
                date: day(n),
                open: close + dec!(0.2),
                high: close + dec!(1.0),
                low: close - dec!(1.0),
                close,
                volume: dec!(100000),
            })
            .unwrap()
        })
        .collect()
}

fn registry(stores: &Stores) -> StrategyRegistry {
    StrategyRegistry::new(
        stores.registry.clone(),
        stores.audit.clone(),
        stores.evidence.clone(),
    )
}

async fn evidence(stores: &Stores, version: StrategyVersionId, kind: EvidenceKind) -> EvidenceId {
    let id = EvidenceId::new_at(FixedClock.now());
    stores
        .evidence
        .record(&EvidenceRecord {
            id,
            version,
            kind,
            passed: true,
            report: json!({"evidence": []}),
            created_at: FixedClock.now(),
        })
        .await
        .unwrap();
    id
}

fn approval(evidence: EvidenceId) -> OwnerApproval {
    OwnerApproval {
        approved_by: UserId::new_at(FixedClock.now()),
        approved_at: FixedClock.now(),
        evidence,
    }
}

struct World {
    pool: PgPool,
    stores: Stores,
    version: StrategyVersionId,
    account: AccountId,
    /// A date on which the strategy enters.
    entry_day: NaiveDate,
}

/// A version promoted to SmallCapital with recorded evidence, a live
/// account, and a date on which the strategy enters (found with a paper run).
async fn world(pool: PgPool) -> World {
    let stores = Stores::new(&pool);
    let spec = spec();
    stores.market.add_instrument(&spec).await.unwrap();
    stores
        .market
        .insert_bars(spec.id, &trending_bars(600), t0())
        .await
        .unwrap();
    let now = FixedClock.now();
    let record = StrategyVersionRecord {
        reference: StrategyRef {
            strategy_id: StrategyId::new_at(now),
            name: "Trend pullback".to_owned(),
            version_id: StrategyVersionId::new_at(now),
            version_number: 1,
            logic_version: TrendPullback::LOGIC_VERSION.to_owned(),
            git_sha: "test".to_owned(),
        },
        parameters: qd_strategy::catalog::catalog()
            .unwrap()
            .remove(0)
            .parameters,
        rr_floor: TrendPullback::v1().params().rr_floor,
    };
    let registry = registry(&stores);
    registry.register(&record, "test").await.unwrap();
    let version = record.reference.version_id;
    let validation = evidence(&stores, version, EvidenceKind::Validation).await;
    for event in [
        StageEvent::StartResearch,
        StageEvent::PassResearch {
            evidence: validation,
        },
        StageEvent::Promote {
            to: TradingStage::Paper,
            approval: approval(validation),
        },
    ] {
        registry.transition(version, &event, "test").await.unwrap();
    }
    let entry_day = first_entry_day(&stores).await;
    let review = evidence(&stores, version, EvidenceKind::PaperReview).await;
    registry
        .transition(
            version,
            &StageEvent::Promote {
                to: TradingStage::SmallCapital,
                approval: approval(review),
            },
            "test",
        )
        .await
        .unwrap();
    let account = AccountId::new_at(now);
    stores
        .accounts
        .create(
            &AccountRecord {
                id: account,
                name: "live".to_owned(),
                mode: AccountMode::Live,
                currency: "INR".to_owned(),
                live_armed: false,
            },
            "test",
        )
        .await
        .unwrap();
    World {
        pool,
        stores,
        version,
        account,
        entry_day,
    }
}

fn risk() -> RiskConfig {
    RiskConfig::new(toml::from_str::<RiskConfigData>(RISK).unwrap()).unwrap()
}

fn costs() -> ScheduleCostModel {
    ScheduleCostModel::new(toml::from_str::<CostScheduleSet>(COSTS).unwrap()).unwrap()
}

/// The first date a paper run of the same data enters on.
async fn first_entry_day(stores: &Stores) -> NaiveDate {
    let paper_account = AccountId::new_at(t0());
    stores
        .accounts
        .create(
            &AccountRecord {
                id: paper_account,
                name: "paper".to_owned(),
                mode: AccountMode::Paper,
                currency: "INR".to_owned(),
                live_armed: false,
            },
            "test",
        )
        .await
        .unwrap();
    let runner = qd_broker_paper::PaperRunner::new(
        qd_broker_paper::PaperDeps {
            journal: stores.journal.clone(),
            reader: stores.journal.clone(),
            halts: stores.halts.clone(),
            market: stores.market.clone(),
            registry: registry(stores),
            accounts: stores.accounts.clone(),
            costs: Arc::new(costs()),
            risk: risk(),
            evidence: Arc::new(FixedEvidence(Arc::new(FakeEvidence))),
            lock: stores.locks.clone(),
            clock: Arc::new(FixedClock),
        },
        settings_paper(paper_account),
    )
    .unwrap();
    let report = runner.run(day(400)).await.unwrap();
    report
        .days
        .iter()
        .find(|d| {
            d.decisions
                .keys()
                .any(|k| k.to_lowercase().contains("open"))
        })
        .map(|d| d.date)
        .expect("the synthetic trend produces an entry")
}

fn settings_paper(account: AccountId) -> qd_broker_paper::PaperSettings {
    qd_broker_paper::PaperSettings {
        account,
        initial_equity: dec!(1000000),
        start: day(230),
        close_time_utc: NaiveTime::from_hms_opt(10, 0, 0).unwrap(),
        slippage_ticks: dec!(1),
        warm_up_days: 400,
        calendar_version: "test".to_owned(),
    }
}

fn live_runner(w: &World, live: LivePolicy) -> LiveRunner {
    LiveRunner::new(
        LiveDeps {
            journal: w.stores.journal.clone(),
            reader: w.stores.journal.clone(),
            halts: w.stores.halts.clone(),
            market: w.stores.market.clone(),
            registry: registry(&w.stores),
            accounts: w.stores.accounts.clone(),
            costs: Arc::new(costs()),
            risk: risk(),
            evidence: Arc::new(FixedEvidence(Arc::new(FakeEvidence))),
            lock: w.stores.locks.clone(),
            clock: Arc::new(FixedClock),
            live,
        },
        LiveSettings {
            account: w.account,
            initial_equity: dec!(1000000),
            start: day(230),
            close_time_utc: NaiveTime::from_hms_opt(10, 0, 0).unwrap(),
            slippage_ticks: dec!(1),
            warm_up_days: 400,
            calendar_version: "test".to_owned(),
        },
    )
    .unwrap()
}

async fn active_halts(w: &World) -> Vec<Halt> {
    w.stores
        .halts
        .load()
        .await
        .unwrap()
        .into_iter()
        .filter(|h| h.is_active_at(FixedClock.now()))
        .collect()
}

// ------------------------------------------------------------------ tests

#[tokio::test]
async fn login_exchanges_the_request_token_signed_with_the_secret() {
    let (base, state) = start_fake_kite().await;
    let session = qd_broker_kite::login::create_session(&client(&base, None), "rt1", "secret1")
        .await
        .unwrap();
    assert_eq!(session.user_id, "AB1234");
    assert_eq!(session.access_token, "tok1");
    assert!(
        !format!("{session:?}").contains("tok1"),
        "Debug hides the token"
    );
    let sent = requests(&state, "POST", "/session/token");
    let body = form(&sent[0].1);
    assert_eq!(body["api_key"], "key1");
    assert_eq!(body["request_token"], "rt1");
    assert_eq!(
        body["checksum"],
        qd_broker_kite::login::checksum("key1", "rt1", "secret1")
    );
    assert!(
        !sent[0].1.contains("secret1"),
        "the secret itself is never sent"
    );
}

#[tokio::test]
async fn candles_need_the_session_and_an_expired_one_asks_for_a_new_login() {
    let (base, _state) = start_fake_kite().await;
    let today = NaiveDate::from_ymd_opt(2026, 9, 25).unwrap();
    let from = NaiveDate::from_ymd_opt(2026, 9, 1).unwrap();
    let bars = qd_broker_kite::market::daily_candles(
        &client(&base, Some("tok1")),
        "1",
        from,
        today,
        false,
        today,
    )
    .await
    .unwrap();
    assert_eq!(bars.len(), 1);
    assert_eq!(bars[0].close().value(), dec!(100.25));
    let expired = qd_broker_kite::market::daily_candles(
        &client(&base, Some("old")),
        "1",
        from,
        today,
        false,
        today,
    )
    .await;
    assert!(matches!(
        expired,
        Err(qd_broker_kite::client::KiteError::Token(_))
    ));
}

#[sqlx::test(migrator = "qd_store::MIGRATOR")]
async fn invariant_14_without_every_live_condition_no_order_reaches_kite(pool: PgPool) {
    let w = world(pool).await;
    let (base, state) = start_fake_kite().await;
    let report = live_runner(&w, LivePolicy::default())
        .run(client(&base, Some("tok1")), SETTINGS, w.entry_day)
        .await
        .unwrap();
    assert_eq!(report.processed, Some(w.entry_day));
    assert!(
        report
            .decisions
            .keys()
            .any(|k| k.to_lowercase().contains("open")),
        "{report:?}"
    );
    // The entry was decided and journaled, then refused by the gateway.
    assert!(requests(&state, "POST", "/orders").is_empty());
    assert!(requests(&state, "POST", "/gtt").is_empty());
    let refused: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM journal WHERE kind = 'order_event' AND entry->'event'->>'event' = 'gateway_rejected'",
    )
    .fetch_one(&w.pool)
    .await
    .unwrap();
    assert!(refused >= 1);
}

#[sqlx::test(migrator = "qd_store::MIGRATOR")]
async fn invariant_11_a_hard_halt_demotes_live_versions_to_paper(pool: PgPool) {
    let w = world(pool).await;
    let (base, _state) = start_fake_kite().await;
    let now = FixedClock.now();
    let halt = Halt::new(
        HaltId::new_at(now),
        HaltKind::HardHalt,
        HaltScope::Account(w.account),
        "drawdown limit",
        now,
        None,
        true,
    )
    .unwrap();
    w.stores.halts.record(&halt).await.unwrap();
    let report = live_runner(&w, LivePolicy::default())
        .run(client(&base, Some("tok1")), SETTINGS, w.entry_day)
        .await
        .unwrap();
    assert_eq!(report.demoted, vec!["Trend pullback v1".to_owned()]);
    let stage = registry(&w.stores).stage(w.version).await.unwrap();
    assert_eq!(stage, StrategyStage::Paper);
    // Demoted before the cycle: no live slot was evaluated.
    assert!(report.decisions.is_empty(), "{report:?}");
}

#[sqlx::test(migrator = "qd_store::MIGRATOR")]
async fn invariant_06_a_book_that_disagrees_with_kite_halts_live_entries(pool: PgPool) {
    let w = world(pool).await;
    let (base, state) = start_fake_kite().await;
    state.lock().unwrap().holdings = vec![json!({
        "tradingsymbol": "TESTEQ", "exchange": "NSE", "quantity": 7, "t1_quantity": 0,
    })];
    let report = live_runner(&w, LivePolicy::default())
        .run(client(&base, Some("tok1")), SETTINGS, w.entry_day)
        .await
        .unwrap();
    assert_eq!(report.mismatches, 1);
    let halts = active_halts(&w).await;
    assert!(
        halts
            .iter()
            .any(|h| h.kind() == HaltKind::Operational && h.reason().contains("reconciliation")),
        "{halts:?}"
    );
}

#[sqlx::test(migrator = "qd_store::MIGRATOR")]
async fn an_expired_kite_session_stops_the_run_before_any_order(pool: PgPool) {
    let w = world(pool).await;
    let (base, state) = start_fake_kite().await;
    let result = live_runner(&w, LivePolicy::default())
        .run(client(&base, Some("old")), SETTINGS, w.entry_day)
        .await;
    assert!(
        matches!(
            result,
            Err(qd_broker_kite::runner::LiveError::Kite(
                qd_broker_kite::client::KiteError::Token(_)
            ))
        ),
        "{result:?}"
    );
    assert!(requests(&state, "POST", "/").is_empty());
}

/// With the feature compiled in and every INV-14 condition met, the entry
/// goes out as an AMO stop-limit order, and its fill gets one two-leg GTT.
#[cfg(feature = "live-orders")]
#[sqlx::test(migrator = "qd_store::MIGRATOR")]
async fn entries_go_out_as_amo_orders_and_a_fill_gets_one_two_leg_gtt(pool: PgPool) {
    use qd_app::live::Environment;
    let w = world(pool).await;
    w.stores
        .accounts
        .set_live_armed(w.account, true, "test")
        .await
        .unwrap();
    let (base, state) = start_fake_kite().await;
    let live = LivePolicy {
        environment: Environment::Production,
        live_trading_enabled: true,
    };
    let runner = live_runner(&w, live);
    runner
        .run(client(&base, Some("tok1")), SETTINGS, w.entry_day)
        .await
        .unwrap();
    let placed = requests(&state, "POST", "/orders/");
    assert_eq!(placed.len(), 1, "{placed:?}");
    assert_eq!(placed[0].0, "/orders/amo");
    let order = form(&placed[0].1);
    assert_eq!(order["exchange"], "NSE");
    assert_eq!(order["tradingsymbol"], "TESTEQ");
    assert_eq!(order["transaction_type"], "BUY");
    assert_eq!(order["order_type"], "SL");
    assert_eq!(order["product"], "CNC");
    assert_eq!(order["validity"], "DAY");
    assert_eq!(order["tag"].len(), 20);
    let trigger: Decimal = order["trigger_price"].parse().unwrap();

    // The order fills at Kite; the intraday sync protects the position.
    {
        let mut s = state.lock().unwrap();
        s.orders = vec![json!({
            "order_id": "O1", "status": "COMPLETE", "variety": "amo",
            "filled_quantity": order["quantity"].parse::<u64>().unwrap(),
            "average_price": trigger, "tag": order["tag"],
            "exchange_update_timestamp": "2027-09-01 09:20:00",
        })];
        s.last_price = Some(trigger);
    }
    let report = runner
        .sync(client(&base, Some("tok1")), SETTINGS)
        .await
        .unwrap();
    assert_eq!(report.refresh.fills, 1, "{report:?}");
    assert_eq!(
        report.refresh.fill_notes,
        vec![format!(
            "BUY {} NSE:TESTEQ @ {} (entry)",
            order["quantity"],
            trigger.normalize()
        )]
    );
    assert_eq!(report.unprotected, 0, "{report:?}");
    let gtts = requests(&state, "POST", "/gtt/triggers");
    assert_eq!(gtts.len(), 1, "stop and target are one OCO order");
    let gtt = form(&gtts[0].1);
    assert_eq!(gtt["type"], "two-leg");
    let condition: Value = serde_json::from_str(&gtt["condition"]).unwrap();
    let orders: Value = serde_json::from_str(&gtt["orders"]).unwrap();
    let number = |v: &Value| v.to_string().trim_matches('"').parse::<Decimal>().unwrap();
    let (stop, target) = (
        number(&condition["trigger_values"][0]),
        number(&condition["trigger_values"][1]),
    );
    assert!(stop < trigger && trigger < target);
    assert_eq!(number(&condition["last_price"]), trigger);
    // Both legs sell the filled quantity; the stop leg's limit sits below the stop.
    for leg in orders.as_array().unwrap() {
        assert_eq!(leg["transaction_type"], "SELL");
        assert_eq!(leg["order_type"], "LIMIT");
        assert_eq!(leg["product"], "CNC");
    }
    assert!(number(&orders[0]["price"]) < stop);
    assert_eq!(number(&orders[1]["price"]), target);
}
