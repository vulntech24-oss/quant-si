//! Shared fixtures for Risk Gate tests. Instruments, probabilities and account
//! figures are test data.

// Test code: a failed unwrap is a failed test. The workspace-wide unwrap/expect
// ban targets runtime code, and clippy's allow-unwrap-in-tests does not reach
// helper functions in integration-test files.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(dead_code)] // each test binary uses a different subset of these fixtures

use std::sync::OnceLock;

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use qd_domain::action::EntryAction;
use qd_domain::costs::{
    CostModel, CostRequest, CostScheduleSet, ScheduleCostModel, VerificationStatus,
};
use qd_domain::economics::{OutcomeProbabilities, SlippageAssumption};
use qd_domain::halt::HaltState;
use qd_domain::ids::{
    AccountId, InstrumentId, ProposalId, SnapshotId, StrategyId, StrategyVersionId,
};
use qd_domain::instrument::{
    AssetClass, CalendarId, Capabilities, CorrelationBucket, InstrumentKind, InstrumentSpec,
    InstrumentSpecData, OrderType, ProductType, ProtectionMode, Validity, Venue,
};
use qd_domain::lifecycle::strategy::StrategyStage;
use qd_domain::num::{Currency, FxRate, Money, Price, Quantity};
use qd_domain::plan::{EntryOrderType, InvalidationRule, TradePlanInput};
use qd_domain::proposal::{
    AccountMode, Explanation, FactorValue, Grade, ProposalDraft, Reason, ReasonDirection,
    Reproducibility, StrategyRef, TradeProposal,
};
use qd_risk::config::{RiskConfig, RiskConfigData};
use qd_risk::gate::{AccountRiskState, EntryRequest, RiskItem};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

pub const COSTS: &str = include_str!("../../../../config/costs/india-zerodha.toml");
pub const RISK: &str = include_str!("../../../../config/risk.toml");

pub fn at(hour: u32, minute: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 3, 16, hour, minute, 0).unwrap()
}

pub fn date() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 3, 16).unwrap()
}

pub fn config() -> RiskConfig {
    RiskConfig::new(toml::from_str::<RiskConfigData>(RISK).unwrap()).unwrap()
}

/// The shipped schedules, marked unverified: for testing that live refuses them.
pub fn costs() -> ScheduleCostModel {
    let mut set: CostScheduleSet = toml::from_str(COSTS).unwrap();
    for schedule in &mut set.schedules {
        schedule.verification.status = VerificationStatus::Unverified;
    }
    ScheduleCostModel::new(set).unwrap()
}

/// The shipped schedules as they are (verified against zerodha.com/charges).
pub fn verified_costs() -> ScheduleCostModel {
    let model = ScheduleCostModel::new(toml::from_str::<CostScheduleSet>(COSTS).unwrap()).unwrap();
    assert!(model.schedules().iter().all(|s| s.is_verified()));
    model
}

pub fn equity_spec() -> InstrumentSpec {
    InstrumentSpec::new(InstrumentSpecData {
        id: InstrumentId::new_at(at(0, 0)),
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
            order_types: vec![OrderType::Limit, OrderType::StopLimit],
            validities: vec![Validity::Day],
        },
    })
    .unwrap()
}

/// The same strategy version on every call (ids contain random bits).
pub fn strategy_ref() -> StrategyRef {
    static IDS: OnceLock<(StrategyId, StrategyVersionId)> = OnceLock::new();
    let (strategy_id, version_id) = *IDS.get_or_init(|| {
        (
            StrategyId::new_at(at(0, 0)),
            StrategyVersionId::new_at(at(0, 0)),
        )
    });
    StrategyRef {
        strategy_id,
        name: "Trend pullback".to_owned(),
        version_id,
        version_number: 1,
        logic_version: "trend-pullback-1.0.0".to_owned(),
        git_sha: "0123abc".to_owned(),
    }
}

/// Builds a proposal with costs quoted at `reference_quantity` from the shipped schedules.
pub fn proposal_with(
    spec: &InstrumentSpec,
    strategy: StrategyRef,
    mode: AccountMode,
    plan: TradePlanInput,
    reference_quantity: Decimal,
) -> TradeProposal {
    let side = plan.action.side();
    let quote = costs()
        .quote(&CostRequest {
            spec,
            product: ProductType::Delivery,
            side,
            quantity: Quantity::new(reference_quantity).unwrap(),
            entry: Price::new(plan.entry).unwrap(),
            exit: Price::new(plan.stop.max(plan.target)).unwrap(),
            trade_date: date(),
        })
        .unwrap();
    TradeProposal::build(
        ProposalDraft {
            id: ProposalId::new_at(at(9, 50)),
            created_at: at(9, 50),
            as_of: at(9, 45),
            trading_date: date(),
            account_mode: mode,
            setup_type: "pullback_in_uptrend".to_owned(),
            grade: Grade::A,
            strategy,
            plan,
            costs: quote.estimate,
            slippage: SlippageAssumption::new("slip-v1", dec!(0.30)).unwrap(),
            probabilities: OutcomeProbabilities::new(
                dec!(0.40),
                dec!(0.45),
                dec!(0.15),
                "test-fixture",
                42,
            )
            .unwrap(),
            time_exit_pnl_per_unit: dec!(1.00),
            explanation: Explanation {
                reasons: vec![Reason {
                    factor: "trend".to_owned(),
                    value: FactorValue::Number(dec!(1)),
                    direction: ReasonDirection::Supports,
                }],
                strongest_argument_against: "Momentum is fading".to_owned(),
                ai_review: None,
            },
            reproducibility: Reproducibility {
                snapshot_id: SnapshotId::new_at(at(9, 45)),
                feature_set_version: "features-v1".to_owned(),
                calendar_version: "nse-test".to_owned(),
            },
        },
        spec,
    )
    .unwrap()
}

pub fn long_plan() -> TradePlanInput {
    TradePlanInput {
        action: EntryAction::OpenLong,
        entry_type: EntryOrderType::StopLimit,
        entry: dec!(100.00),
        stop: dec!(95.50),
        target: dec!(112.70),
        max_holding_days: 15,
        invalidation: vec![InvalidationRule::RegimeChange],
    }
}

pub fn short_plan() -> TradePlanInput {
    TradePlanInput {
        action: EntryAction::OpenShort,
        entry_type: EntryOrderType::StopLimit,
        entry: dec!(100.00),
        stop: dec!(104.50),
        target: dec!(87.30),
        max_holding_days: 15,
        invalidation: vec![],
    }
}

pub fn inr(amount: Decimal) -> Money {
    Money::new(amount, Currency::INR)
}

/// A healthy paper account with 10,00,000 INR and nothing open.
pub fn account(mode: AccountMode) -> AccountRiskState {
    AccountRiskState {
        account: AccountId::new_at(at(0, 0)),
        mode,
        equity: inr(dec!(1000000)),
        equity_at_day_start: inr(dec!(1000000)),
        equity_at_week_start: inr(dec!(1000000)),
        high_water_mark: inr(dec!(1000000)),
        consecutive_losses: 0,
        open_risk: vec![],
        halts: HaltState::Known(vec![]),
    }
}

pub fn request<'a>(
    proposal: &'a TradeProposal,
    spec: &'a InstrumentSpec,
    stage: StrategyStage,
) -> EntryRequest<'a> {
    EntryRequest {
        proposal,
        spec,
        product: ProductType::Delivery,
        stage,
        rr_floor: dec!(2.0),
        fx: FxRate::identity(Currency::INR, at(9, 0)),
        at: at(10, 0),
    }
}

pub fn risk_item(
    bucket: &str,
    strategy_version: Option<StrategyVersionId>,
    amount: Decimal,
) -> RiskItem {
    RiskItem {
        instrument: InstrumentId::new_at(at(1, 0)),
        strategy_version,
        bucket: CorrelationBucket(bucket.to_owned()),
        amount: inr(amount),
    }
}
