//! Paper trading over PostgreSQL: restore from the journal, idempotent
//! resumption, fail-closed restore, halts and evidence. Synthetic test data,
//! not market data.

// Test code: a failed unwrap is a failed test. The workspace-wide unwrap/expect
// ban targets runtime code, and clippy's allow-unwrap-in-tests does not reach
// helper functions in integration-test files.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use chrono::{DateTime, Days, NaiveDate, NaiveTime, TimeZone, Utc};
use qd_app::evidence::{EvidenceKind, EvidenceRecord, EvidenceStore};
use qd_app::ports::{
    Clock, Evidence, EvidenceSource, HaltStore, PaperTrading, Reconciler, RunLock,
};
use qd_app::registry::{StrategyRegistry, StrategyVersionRecord};
use qd_broker_paper::runner::{PaperError, catalog};
use qd_broker_paper::{NoEvidenceTables, PaperDeps, PaperRunner, PaperSettings};
use qd_domain::costs::{CostScheduleSet, ScheduleCostModel};
use qd_domain::economics::OutcomeProbabilities;
use qd_domain::halt::{Halt, HaltKind, HaltScope};
use qd_domain::ids::{
    AccountId, EvidenceId, HaltId, InstrumentId, StrategyId, StrategyVersionId, UserId,
};
use qd_domain::instrument::{
    AssetClass, CalendarId, Capabilities, CorrelationBucket, InstrumentKind, InstrumentSpec,
    InstrumentSpecData, OrderType, ProductType, ProtectionMode, Validity, Venue,
};
use qd_domain::lifecycle::strategy::{OwnerApproval, StageEvent, TradingStage};
use qd_domain::market::{Bar, BarData};
use qd_domain::num::Currency;
use qd_domain::proposal::{AccountMode, StrategyRef};
use qd_risk::config::{RiskConfig, RiskConfigData};
use qd_store::{AccountRecord, Stores};
use qd_strategy::trend_pullback::TrendPullback;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde_json::Value;
use sqlx::PgPool;

const COSTS: &str = include_str!("../../../config/costs/india-zerodha.toml");
const RISK: &str = include_str!("../../../config/risk.toml");

/// The wall clock of the tests: after all the synthetic bars.
struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2027, 9, 1, 12, 0, 0).unwrap()
    }
}

/// Evidence as validation would produce it (Phase 7): enough setups, positive EV.
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
        broker_refs: vec![],
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

struct World {
    pool: PgPool,
    stores: Stores,
    version: StrategyVersionId,
}

async fn world(pool: PgPool) -> World {
    let stores = Stores::new(&pool);
    let spec = spec();
    stores.market.add_instrument(&spec).await.unwrap();
    stores
        .market
        .insert_bars(spec.id, &trending_bars(600), t0())
        .await
        .unwrap();
    let parameters = catalog().unwrap().remove(0).parameters;
    let version = register(&stores, parameters).await;
    World {
        pool,
        stores,
        version,
    }
}

async fn register(stores: &Stores, parameters: Value) -> StrategyVersionId {
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
        parameters,
        rr_floor: TrendPullback::v1().params().rr_floor,
    };
    let registry = registry(stores);
    registry.register(&record, "test").await.unwrap();
    let id = record.reference.version_id;
    let evidence = EvidenceId::new_at(now);
    stores
        .evidence
        .record(&EvidenceRecord {
            id: evidence,
            version: id,
            kind: EvidenceKind::Validation,
            passed: true,
            report: serde_json::json!({"evidence": []}),
            created_at: now,
        })
        .await
        .unwrap();
    for event in [
        StageEvent::StartResearch,
        StageEvent::PassResearch { evidence },
        StageEvent::Promote {
            to: TradingStage::Paper,
            approval: OwnerApproval {
                approved_by: UserId::new_at(now),
                approved_at: now,
                evidence,
            },
        },
    ] {
        registry.transition(id, &event, "test").await.unwrap();
    }
    id
}

fn registry(stores: &Stores) -> StrategyRegistry {
    StrategyRegistry::new(
        stores.registry.clone(),
        stores.audit.clone(),
        stores.evidence.clone(),
    )
}

async fn account(stores: &Stores, mode: AccountMode) -> AccountId {
    let id = AccountId::new_at(FixedClock.now());
    stores
        .accounts
        .create(
            &AccountRecord {
                id,
                name: "test".to_owned(),
                mode,
                currency: "INR".to_owned(),
                live_armed: false,
            },
            "test",
        )
        .await
        .unwrap();
    id
}

fn runner(w: &World, account: AccountId, evidence: Arc<dyn EvidenceSource>) -> PaperRunner {
    let evidence = Arc::new(qd_app::evidence::FixedEvidence(evidence));
    let risk = RiskConfig::new(toml::from_str::<RiskConfigData>(RISK).unwrap()).unwrap();
    let costs = ScheduleCostModel::new(toml::from_str::<CostScheduleSet>(COSTS).unwrap()).unwrap();
    PaperRunner::new(
        PaperDeps {
            journal: w.stores.journal.clone(),
            reader: w.stores.journal.clone(),
            halts: w.stores.halts.clone(),
            market: w.stores.market.clone(),
            registry: registry(&w.stores),
            accounts: w.stores.accounts.clone(),
            costs: Arc::new(costs),
            risk,
            evidence,
            lock: w.stores.locks.clone(),
            clock: Arc::new(FixedClock),
        },
        PaperSettings {
            account,
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

/// The `day_closed` entries of an account: (date, equity, trades without ids).
async fn days(pool: &PgPool, account: AccountId) -> Vec<(String, String, Vec<Value>)> {
    let rows: Vec<Value> = sqlx::query_scalar(
        "SELECT entry FROM journal WHERE kind = 'day_closed' AND entry->>'account' = $1 ORDER BY seq",
    )
    .bind(account.to_string())
    .fetch_all(pool)
    .await
    .unwrap();
    rows.into_iter()
        .map(|e| {
            let trades = e["trades"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| {
                    let mut t = t.clone();
                    let t_obj = t.as_object_mut().unwrap();
                    t_obj.remove("position");
                    t_obj.remove("decision");
                    t
                })
                .collect();
            (e["date"].to_string(), e["equity"].to_string(), trades)
        })
        .collect()
}

async fn count(pool: &PgPool, kind: &str, account: AccountId) -> i64 {
    let path = if kind == "order_intent" {
        "entry->'intent'->>'account'"
    } else {
        "entry->>'account'"
    };
    sqlx::query_scalar(&format!(
        "SELECT count(*) FROM journal WHERE kind = $1 AND {path} = $2"
    ))
    .bind(kind)
    .bind(account.to_string())
    .fetch_one(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "qd_store::MIGRATOR")]
async fn invariant_08_a_restarted_paper_run_matches_an_uninterrupted_one(pool: PgPool) {
    let w = world(pool).await;
    let straight = account(&w.stores, AccountMode::Paper).await;
    let split = account(&w.stores, AccountMode::Paper).await;

    let report = runner(&w, straight, Arc::new(FakeEvidence))
        .run(day(599))
        .await
        .unwrap();
    assert!(report.skipped_versions.is_empty(), "{report:?}");
    // Two runners with nothing shared in memory: the second restores from the journal.
    let first = runner(&w, split, Arc::new(FakeEvidence))
        .run(day(300))
        .await
        .unwrap();
    let second = runner(&w, split, Arc::new(FakeEvidence))
        .run(day(407))
        .await
        .unwrap();
    let third = runner(&w, split, Arc::new(FakeEvidence))
        .run(day(599))
        .await
        .unwrap();
    assert_eq!(first.resumed_after, None);
    assert_eq!(second.resumed_after, Some(day(300)));
    assert_eq!(third.resumed_after, Some(day(407)));
    assert_eq!(
        first.days.len() + second.days.len() + third.days.len(),
        report.days.len()
    );
    // The restored venue agrees with the restored book on every day.
    for d in report
        .days
        .iter()
        .chain(&first.days)
        .chain(&second.days)
        .chain(&third.days)
    {
        assert_eq!((d.mismatches, d.unprotected), (0, 0), "{d:?}");
    }
    assert!(
        second.active_positions + third.active_positions > 0 || first.active_positions > 0,
        "a restart should happen with a position open at least once"
    );

    let a = days(&w.pool, straight).await;
    let b = days(&w.pool, split).await;
    assert_eq!(a, b);
    let trades: usize = a.iter().map(|d| d.2.len()).sum();
    assert!(trades >= 5, "the synthetic trend should trade: {trades}");

    // Running again processes nothing and journals nothing new.
    let again = runner(&w, split, Arc::new(FakeEvidence))
        .run(day(599))
        .await
        .unwrap();
    assert!(again.days.is_empty());
    assert_eq!(days(&w.pool, split).await.len(), b.len());

    // The restored book reconciles with the paper venue, and its state reads back.
    let r = runner(&w, split, Arc::new(FakeEvidence));
    assert!(r.reconcile().await.unwrap().is_empty());
    let state = r.state().await.unwrap();
    assert!(state["inconsistency"].is_null());
    assert_eq!(state["last_day"]["date"], day(599).to_string());
}

#[sqlx::test(migrator = "qd_store::MIGRATOR")]
async fn invariant_06_a_book_the_orders_contradict_is_refused_and_halts(pool: PgPool) {
    let w = world(pool).await;
    let acct = account(&w.stores, AccountMode::Paper).await;
    runner(&w, acct, Arc::new(FakeEvidence))
        .run(day(400))
        .await
        .unwrap();
    // Tamper: a snapshot claiming more than the fills add up to.
    let mut snapshot: Value = sqlx::query_scalar(
        "SELECT entry FROM journal WHERE kind = 'position_snapshot' \
         AND entry->'position'->>'account' = $1 AND entry->'position'->>'quantity' <> '0' \
         ORDER BY seq DESC LIMIT 1",
    )
    .bind(acct.to_string())
    .fetch_one(&w.pool)
    .await
    .unwrap();
    snapshot["position"]["quantity"] = Value::String("999999".to_owned());
    sqlx::query("INSERT INTO journal (kind, entry) VALUES ('position_snapshot', $1)")
        .bind(&snapshot)
        .execute(&w.pool)
        .await
        .unwrap();

    let r = runner(&w, acct, Arc::new(FakeEvidence));
    assert!(!r.reconcile().await.unwrap().is_empty());
    let err = r.run(day(599)).await.unwrap_err();
    assert!(matches!(err, PaperError::Restore(_)), "{err}");
    let halts = w.stores.halts.load().await.unwrap();
    assert!(halts.iter().any(|h| h.kind() == HaltKind::Operational
        && h.scope() == HaltScope::Account(acct)
        && h.is_active_at(FixedClock.now())));
}

#[sqlx::test(migrator = "qd_store::MIGRATOR")]
async fn invariant_02_a_halt_set_now_blocks_entries_in_a_catch_up_run(pool: PgPool) {
    let w = world(pool).await;
    let acct = account(&w.stores, AccountMode::Paper).await;
    let now = FixedClock.now();
    let halt = Halt::new(
        HaltId::new_at(now),
        HaltKind::Manual,
        HaltScope::Global,
        "owner halt",
        now,
        None,
        true,
    )
    .unwrap();
    w.stores.halts.record(&halt).await.unwrap();
    // Every processed date is in the past; the halt still applies.
    let report = runner(&w, acct, Arc::new(FakeEvidence))
        .run(day(599))
        .await
        .unwrap();
    assert!(!report.days.is_empty());
    assert_eq!(count(&w.pool, "order_intent", acct).await, 0);
    assert!(
        report
            .days
            .iter()
            .any(|d| d.decisions.contains_key("kill_switch")),
        "{report:?}"
    );
}

#[sqlx::test(migrator = "qd_store::MIGRATOR")]
async fn invariant_06_paper_decisions_without_evidence_are_no_trade(pool: PgPool) {
    let w = world(pool).await;
    let acct = account(&w.stores, AccountMode::Paper).await;
    let report = runner(&w, acct, Arc::new(NoEvidenceTables))
        .run(day(599))
        .await
        .unwrap();
    assert_eq!(count(&w.pool, "order_intent", acct).await, 0);
    let codes: Vec<&String> = report
        .days
        .iter()
        .flat_map(|d| d.decisions.keys())
        .collect();
    assert!(!codes.is_empty());
    assert!(
        codes.iter().all(|c| *c == "insufficient_evidence"),
        "{codes:?}"
    );
}

#[sqlx::test(migrator = "qd_store::MIGRATOR")]
async fn paper_runs_refuse_live_accounts_and_concurrent_runs(pool: PgPool) {
    let w = world(pool).await;
    let live = account(&w.stores, AccountMode::Live).await;
    let err = runner(&w, live, Arc::new(FakeEvidence))
        .run(day(599))
        .await
        .unwrap_err();
    assert_eq!(err, PaperError::NotPaperAccount);

    let acct = account(&w.stores, AccountMode::Paper).await;
    let _held = w
        .stores
        .locks
        .try_acquire(&format!("paper-run:{acct}"))
        .await
        .unwrap()
        .unwrap();
    let err = runner(&w, acct, Arc::new(FakeEvidence))
        .run(day(599))
        .await
        .unwrap_err();
    assert_eq!(err, PaperError::Busy);
}

#[sqlx::test(migrator = "qd_store::MIGRATOR")]
async fn invariant_10_a_version_whose_parameters_differ_from_the_code_is_not_run(pool: PgPool) {
    let w = world(pool).await;
    let _ = w.version;
    register(&w.stores, serde_json::json!({"changed": true})).await;
    let acct = account(&w.stores, AccountMode::Paper).await;
    let report = runner(&w, acct, Arc::new(FakeEvidence))
        .run(day(300))
        .await
        .unwrap();
    assert_eq!(report.skipped_versions.len(), 1, "{report:?}");
    assert!(report.skipped_versions[0].contains("parameters"));
}
