//! End to end: validation over stored data records evidence; only passed
//! evidence promotes a version; paper decisions then use the evidence tables.
//! Synthetic test data, not market data.

// Test code: a failed unwrap is a failed test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use chrono::{DateTime, Days, NaiveDate, NaiveTime, TimeZone, Utc};
use qd_app::evidence::{EvidenceStore, StoreEvidenceLoader};
use qd_app::ports::{Clock, ValidationRequest, Validator};
use qd_app::registry::{RegistryError, StrategyRegistry, StrategyVersionRecord};
use qd_app::review::{JournalReviewer, ReviewCriteria, Reviewer};
use qd_backtest::validation::ValidationCriteria;
use qd_backtest::validator::StoreValidator;
use qd_broker_paper::{PaperDeps, PaperRunner, PaperSettings};
use qd_domain::costs::{CostScheduleSet, ScheduleCostModel};
use qd_domain::ids::{AccountId, EvidenceId, InstrumentId, StrategyId, StrategyVersionId, UserId};
use qd_domain::instrument::{
    AssetClass, CalendarId, Capabilities, CorrelationBucket, InstrumentKind, InstrumentSpec,
    InstrumentSpecData, OrderType, ProductType, ProtectionMode, Validity, Venue,
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
use sqlx::PgPool;

const COSTS: &str = include_str!("../../../config/costs/india-zerodha.toml");
const RISK: &str = include_str!("../../../config/risk.toml");
const CRITERIA: &str = include_str!("../../../config/validation.toml");
const REVIEW: &str = include_str!("../../../config/review.toml");

struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2027, 9, 1, 12, 0, 0).unwrap()
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

fn criteria() -> ValidationCriteria {
    let mut c: ValidationCriteria = toml::from_str(CRITERIA).unwrap();
    // The synthetic series is short.
    c.min_holdout_trades = 3;
    c.min_oos_trades = 10;
    c
}

fn risk() -> RiskConfig {
    let mut data: RiskConfigData = toml::from_str(RISK).unwrap();
    // As many comparable setups as the short series offers.
    data.min_evidence = 10;
    RiskConfig::new(data).unwrap()
}

fn costs() -> Arc<ScheduleCostModel> {
    Arc::new(ScheduleCostModel::new(toml::from_str::<CostScheduleSet>(COSTS).unwrap()).unwrap())
}

#[sqlx::test(migrator = "qd_store::MIGRATOR")]
async fn invariant_11_validation_evidence_promotes_and_feeds_paper_decisions(pool: PgPool) {
    let stores = Stores::new(&pool);
    let spec = spec();
    stores.market.add_instrument(&spec).await.unwrap();
    stores
        .market
        .insert_bars(spec.id, &trending_bars(600), t0())
        .await
        .unwrap();
    let registry = StrategyRegistry::new(
        stores.registry.clone(),
        stores.audit.clone(),
        stores.evidence.clone(),
    );
    let now = FixedClock.now();
    let version = StrategyVersionRecord {
        reference: StrategyRef {
            strategy_id: StrategyId::new_at(now),
            name: "Trend pullback".to_owned(),
            version_id: StrategyVersionId::new_at(now),
            version_number: 1,
            logic_version: TrendPullback::LOGIC_VERSION.to_owned(),
            git_sha: "test".to_owned(),
        },
        parameters: serde_json::to_value(TrendPullback::v1().params()).unwrap(),
        rr_floor: TrendPullback::v1().params().rr_floor,
    };
    registry.register(&version, "owner").await.unwrap();
    let id = version.reference.version_id;
    registry
        .transition(id, &StageEvent::StartResearch, "owner")
        .await
        .unwrap();

    let validator = |criteria| {
        StoreValidator::new(
            stores.market.clone(),
            registry.clone(),
            stores.evidence.clone(),
            stores.audit.clone(),
            costs(),
            risk(),
            criteria,
            Arc::new(FixedClock),
        )
        .unwrap()
    };
    let request = ValidationRequest {
        version: id,
        instruments: vec![],
        from: day(230),
        to: day(599),
        equity: dec!(1000000),
    };
    // A failed validation is recorded, and cannot back the stage event.
    let mut strict = criteria();
    strict.min_oos_expectancy_r = dec!(5);
    let failed = validator(strict).validate(&request, "owner").await.unwrap();
    assert_eq!(failed["passed"], false);
    let failed_id: EvidenceId = serde_json::from_value(failed["id"].clone()).unwrap();
    let err = registry
        .transition(
            id,
            &StageEvent::PassResearch {
                evidence: failed_id,
            },
            "owner",
        )
        .await;
    assert!(matches!(err, Err(RegistryError::Evidence(_))), "{err:?}");

    let passed = validator(criteria())
        .validate(&request, "owner")
        .await
        .unwrap();
    assert_eq!(passed["passed"], true, "{:#}", passed["report"]["checks"]);
    let evidence: EvidenceId = serde_json::from_value(passed["id"].clone()).unwrap();
    assert_eq!(stores.evidence.list(Some(id)).await.unwrap().len(), 2);
    registry
        .transition(id, &StageEvent::PassResearch { evidence }, "owner")
        .await
        .unwrap();
    let approval = OwnerApproval {
        approved_by: UserId::new_at(now),
        approved_at: now,
        evidence,
    };
    let stage = registry
        .transition(
            id,
            &StageEvent::Promote {
                to: TradingStage::Paper,
                approval,
            },
            "owner",
        )
        .await
        .unwrap();
    assert_eq!(stage, StrategyStage::Paper);
    // Validation evidence never promotes to a live stage.
    assert!(
        registry
            .transition(
                id,
                &StageEvent::Promote {
                    to: TradingStage::SmallCapital,
                    approval,
                },
                "owner",
            )
            .await
            .is_err()
    );

    // Paper trading now decides with the stored evidence tables.
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
            "owner",
        )
        .await
        .unwrap();
    let runner = PaperRunner::new(
        PaperDeps {
            journal: stores.journal.clone(),
            reader: stores.journal.clone(),
            halts: stores.halts.clone(),
            market: stores.market.clone(),
            registry: registry.clone(),
            accounts: stores.accounts.clone(),
            costs: costs(),
            risk: risk(),
            evidence: Arc::new(StoreEvidenceLoader(stores.evidence.clone())),
            lock: stores.locks.clone(),
            clock: Arc::new(FixedClock),
        },
        PaperSettings {
            account,
            initial_equity: dec!(1000000),
            start: day(530),
            close_time_utc: NaiveTime::from_hms_opt(10, 0, 0).unwrap(),
            slippage_ticks: dec!(1),
            warm_up_days: 400,
            calendar_version: "test".to_owned(),
        },
    )
    .unwrap();
    let report = runner.run(day(599)).await.unwrap();
    let entered: u32 = report
        .days
        .iter()
        .filter_map(|d| d.decisions.get("BUY (open long)"))
        .sum();
    let no_evidence: u32 = report
        .days
        .iter()
        .filter_map(|d| d.decisions.get("insufficient_evidence"))
        .sum();
    assert!(entered > 0, "{report:?}");
    assert_eq!(no_evidence, 0, "{report:?}");

    // Review: paper predictions meet paper outcomes. The shipped criteria
    // need far more paper history than this, so the recorded review fails,
    // and a failed review cannot promote to a live stage.
    let reviewer = |criteria| JournalReviewer {
        reader: stores.journal.clone(),
        evidence: stores.evidence.clone(),
        audit: stores.audit.clone(),
        clock: Arc::new(FixedClock),
        account,
        criteria,
    };
    let shipped: ReviewCriteria = toml::from_str(REVIEW).unwrap();
    let summary = reviewer(shipped.clone()).review().await.unwrap();
    let v = &summary["report"]["versions"][0];
    assert!(v["trades"].as_u64().unwrap() > 0, "{summary:#}");
    let failed = reviewer(shipped.clone()).record(id, "owner").await.unwrap();
    assert_eq!(failed["passed"], false);
    let failed_review: EvidenceId = serde_json::from_value(failed["id"].clone()).unwrap();
    let promote = |evidence| StageEvent::Promote {
        to: TradingStage::SmallCapital,
        approval: OwnerApproval {
            approved_by: UserId::new_at(now),
            approved_at: now,
            evidence,
        },
    };
    assert!(
        registry
            .transition(id, &promote(failed_review), "owner")
            .await
            .is_err()
    );
    // With criteria this short history can meet, a passed review backs the promotion.
    let lenient = ReviewCriteria {
        min_trades: 1,
        min_days: 1,
        min_expectancy_r: Decimal::ZERO,
        max_expectancy_shortfall_r: dec!(10),
        max_drawdown: dec!(0.5),
        max_brier: Decimal::ONE,
    };
    let passed = reviewer(lenient).record(id, "owner").await.unwrap();
    assert_eq!(passed["passed"], true, "{passed:#}");
    let review_id: EvidenceId = serde_json::from_value(passed["id"].clone()).unwrap();
    assert_eq!(
        registry
            .transition(id, &promote(review_id), "owner")
            .await
            .unwrap(),
        StrategyStage::SmallCapital
    );
}
