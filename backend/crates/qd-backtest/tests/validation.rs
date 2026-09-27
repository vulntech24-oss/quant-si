//! Validation protocol, evidence tables and Monte Carlo, on deterministic
//! synthetic data (test data, not market data).

// Test code: a failed unwrap is a failed test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use chrono::{Days, NaiveDate, NaiveTime, TimeZone, Utc};
use qd_app::decision::StrategyVersionInfo;
use qd_app::evidence::evidence_tables;
use qd_backtest::engine::InstrumentData;
use qd_backtest::metrics::TradeRecord;
use qd_backtest::montecarlo::{MonteCarloConfig, monte_carlo};
use qd_backtest::validation::{ValidationCriteria, ValidationInput, validate};
use qd_domain::action::Side;
use qd_domain::costs::{CostScheduleSet, ScheduleCostModel};
use qd_domain::economics::SlippageAssumption;
use qd_domain::ids::{InstrumentId, PositionId, StrategyId, StrategyVersionId};
use qd_domain::instrument::{
    AssetClass, CalendarId, Capabilities, CorrelationBucket, InstrumentKind, InstrumentSpec,
    InstrumentSpecData, OrderType, ProductType, ProtectionMode, Validity, Venue,
};
use qd_domain::lifecycle::strategy::StrategyStage;
use qd_domain::market::{Bar, BarData, BarSeries};
use qd_domain::num::{Currency, Money, Quantity};
use qd_domain::outcome::ExitReason;
use qd_domain::proposal::StrategyRef;
use qd_risk::config::{RiskConfig, RiskConfigData};
use qd_strategy::trend_pullback::TrendPullback;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

const COSTS: &str = include_str!("../../../config/costs/india-zerodha.toml");
const RISK: &str = include_str!("../../../config/risk.toml");
const CRITERIA: &str = include_str!("../../../config/validation.toml");

fn day(n: usize) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 1, 1).unwrap() + Days::new(n as u64)
}

fn spec() -> InstrumentSpec {
    InstrumentSpec::new(InstrumentSpecData {
        id: InstrumentId::new_at(Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap()),
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
fn trending_bars(len: usize) -> Vec<Bar> {
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

/// A steady downtrend: the long-only strategy never finds a setup.
fn falling_bars(len: usize) -> Vec<Bar> {
    (0..len)
        .map(|n| {
            let close = dec!(300) - dec!(0.3) * Decimal::from(n);
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

fn data(bars: Vec<Bar>) -> Vec<InstrumentData> {
    let spec = spec();
    let series = BarSeries::new(spec.id, bars.last().unwrap().date(), bars).unwrap();
    vec![InstrumentData {
        spec,
        product: ProductType::Delivery,
        series,
    }]
}

fn info() -> StrategyVersionInfo {
    let at = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
    StrategyVersionInfo {
        reference: StrategyRef {
            strategy_id: StrategyId::new_at(at),
            name: "Trend pullback".to_owned(),
            version_id: StrategyVersionId::new_at(at),
            version_number: 1,
            logic_version: TrendPullback::LOGIC_VERSION.to_owned(),
            git_sha: "test".to_owned(),
        },
        stage: StrategyStage::Research,
        rr_floor: TrendPullback::v1().params().rr_floor,
        slippage: SlippageAssumption::new("slip-v1", Decimal::ZERO).unwrap(),
    }
}

fn criteria() -> ValidationCriteria {
    let mut c: ValidationCriteria = toml::from_str(CRITERIA).unwrap();
    // The synthetic series is short: 370 decision days, a 74-day holdout.
    c.min_holdout_trades = 3;
    c.min_oos_trades = 10;
    c
}

fn risk() -> RiskConfig {
    RiskConfig::new(toml::from_str::<RiskConfigData>(RISK).unwrap()).unwrap()
}

fn costs() -> ScheduleCostModel {
    ScheduleCostModel::new(toml::from_str::<CostScheduleSet>(COSTS).unwrap()).unwrap()
}

async fn run(
    bars: Vec<Bar>,
    criteria: &ValidationCriteria,
) -> qd_backtest::validation::ValidationReport {
    let data = data(bars);
    let strategy = TrendPullback::v1();
    let info = info();
    let input = ValidationInput {
        strategy: &strategy,
        info: &info,
        instruments: &data,
        from: day(230),
        to: day(599),
        equity: Money::new(dec!(1000000), Currency::INR),
        slippage_ticks: dec!(1),
        close_time_utc: NaiveTime::from_hms_opt(10, 0, 0).unwrap(),
    };
    validate(&input, criteria, &risk(), &costs()).await.unwrap()
}

#[test]
fn the_shipped_criteria_are_valid() {
    let c: ValidationCriteria = toml::from_str(CRITERIA).unwrap();
    c.validate().unwrap();
    assert_eq!(
        c.min_oos_trades, 30,
        "matches the Decision Engine's min_evidence"
    );
}

#[tokio::test]
async fn a_consistent_edge_passes_and_its_evidence_comes_from_out_of_sample_trades_only() {
    let report = run(trending_bars(600), &criteria()).await;
    assert!(report.passed, "{:#?}", report.checks);
    // Windows tile the development period without overlap, before the holdout.
    assert_eq!(report.windows.first().unwrap().start, day(230));
    for pair in report.windows.windows(2) {
        assert_eq!(pair[0].end + Days::new(1), pair[1].start);
    }
    assert!(report.windows.last().unwrap().end < report.holdout_from);
    assert!(
        report
            .oos_trades
            .iter()
            .all(|t| t.closed_on < report.holdout_from)
    );
    assert!(report.holdout.trades > 0);
    // Evidence counts exactly the out-of-sample trades, and its shares sum to one.
    let total: u32 = report.evidence.iter().map(|t| t.count).sum();
    assert_eq!(total, report.oos.trades);
    for t in &report.evidence {
        assert_eq!(t.p_target + t.p_stop + t.p_time, Decimal::ONE);
    }
    // The same inputs give the same report (seeded Monte Carlo included).
    let again = run(trending_bars(600), &criteria()).await;
    assert_eq!(again.monte_carlo, report.monte_carlo);
    assert_eq!(again.evidence, report.evidence);
}

#[tokio::test]
async fn no_setups_means_no_evidence_and_a_failed_validation() {
    let report = run(falling_bars(600), &criteria()).await;
    assert!(!report.passed);
    assert!(report.evidence.is_empty());
    let failed: Vec<&str> = report
        .checks
        .iter()
        .filter(|c| !c.passed)
        .map(|c| c.name)
        .collect();
    assert!(failed.contains(&"oos_trades"), "{failed:?}");
    assert!(failed.contains(&"monte_carlo"), "{failed:?}");
}

#[tokio::test]
async fn each_threshold_can_fail_a_validation() {
    let mut strict = criteria();
    strict.min_oos_expectancy_r = dec!(5);
    let report = run(trending_bars(600), &strict).await;
    assert!(!report.passed);
    assert_eq!(
        report
            .checks
            .iter()
            .filter(|c| !c.passed)
            .map(|c| c.name)
            .collect::<Vec<_>>(),
        vec!["oos_expectancy"]
    );
    let mut strict = criteria();
    strict.min_holdout_trades = 1000;
    assert!(!run(trending_bars(600), &strict).await.passed);
}

fn trade(reason: ExitReason, r: Decimal) -> TradeRecord {
    let at = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    TradeRecord {
        position: PositionId::new_at(at),
        decision: None,
        setup_type: "s".to_owned(),
        instrument: "X".to_owned(),
        side: Side::Long,
        opened_on: day(0),
        closed_on: day(1),
        quantity: Quantity::new(Decimal::ONE).unwrap(),
        entry_price: dec!(100),
        exit_price: dec!(100),
        gross_pnl: r,
        costs: Decimal::ZERO,
        net_pnl: r,
        r_multiple: r,
        exit_reason: reason,
    }
}

#[test]
fn evidence_tables_round_conservatively_and_sum_to_one() {
    // 1 target, 1 stop, 1 time exit: thirds that do not terminate in decimal.
    let trades = [
        trade(ExitReason::TargetHit, dec!(2)),
        trade(ExitReason::StopHit, dec!(-1)),
        trade(ExitReason::TimeExit, dec!(0.4)),
    ];
    let t = &evidence_tables(&trades)[0];
    assert_eq!(t.p_target, dec!(0.3333)); // rounded down
    assert_eq!(t.p_stop, dec!(0.3334)); // rounded up
    assert_eq!(t.p_target + t.p_stop + t.p_time, Decimal::ONE);
    assert_eq!(t.time_exit_r, dec!(0.4));
    assert_eq!(t.count, 3);
}

#[test]
fn monte_carlo_is_deterministic_and_worse_edges_draw_down_more() {
    let config = MonteCarloConfig {
        paths: 2000,
        seed: 7,
    };
    let good: Vec<Decimal> = (0..50)
        .map(|i| if i % 2 == 0 { dec!(2) } else { dec!(-1) })
        .collect();
    let bad: Vec<Decimal> = (0..50)
        .map(|i| if i % 3 == 0 { dec!(1) } else { dec!(-1) })
        .collect();
    let a = monte_carlo(&good, dec!(0.01), dec!(0.2), config).unwrap();
    let b = monte_carlo(&good, dec!(0.01), dec!(0.2), config).unwrap();
    assert_eq!(a, b);
    let worse = monte_carlo(&bad, dec!(0.01), dec!(0.2), config).unwrap();
    assert!(worse.max_drawdown_p95 > a.max_drawdown_p95);
    assert!(worse.final_return_p50 < a.final_return_p50);
    assert!(a.max_drawdown_p50 <= a.max_drawdown_p95 && a.max_drawdown_p95 <= a.max_drawdown_p99);
    assert!(monte_carlo(&[], dec!(0.01), dec!(0.2), config).is_none());
    assert!(monte_carlo(&good, dec!(1.5), dec!(0.2), config).is_none());
}
