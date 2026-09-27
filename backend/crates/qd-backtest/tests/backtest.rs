//! Backtest engine and simulated broker, on deterministic synthetic data
//! (test data, not market data).

// Test code: a failed unwrap is a failed test. The workspace-wide unwrap/expect
// ban targets runtime code, and clippy's allow-unwrap-in-tests does not reach
// helper functions in integration-test files.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use chrono::{Days, NaiveDate, NaiveTime, TimeZone, Utc};
use qd_app::decision::{EvidencePolicy, StrategyVersionInfo};
use qd_app::orders::OrderTerms;
use qd_app::ports::{Evidence, EvidenceSource};
use qd_backtest::engine::{BacktestConfig, InstrumentData, research_risk_config, run_backtest};
use qd_backtest::sim::simulate_fill;
use qd_domain::action::TradeAction;
use qd_domain::costs::{CostScheduleSet, ScheduleCostModel};
use qd_domain::economics::SlippageAssumption;
use qd_domain::ids::{InstrumentId, StrategyId, StrategyVersionId};
use qd_domain::instrument::{
    AssetClass, CalendarId, Capabilities, CorrelationBucket, InstrumentKind, InstrumentSpec,
    InstrumentSpecData, OrderType, ProductType, ProtectionMode, Validity, Venue,
};
use qd_domain::lifecycle::strategy::StrategyStage;
use qd_domain::market::{Bar, BarData, BarSeries};
use qd_domain::num::Price;
use qd_domain::num::{Currency, Money};
use qd_domain::proposal::StrategyRef;
use qd_risk::config::{RiskConfig, RiskConfigData};
use qd_strategy::trend_pullback::TrendPullback;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

const COSTS: &str = include_str!("../../../config/costs/india-zerodha.toml");
const RISK: &str = include_str!("../../../config/risk.toml");

struct NoEvidence;

impl EvidenceSource for NoEvidence {
    fn evidence(&self, _: StrategyVersionId, _: &str) -> Option<Evidence> {
        None
    }
}

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

fn config() -> BacktestConfig {
    let at = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
    BacktestConfig {
        initial_equity: Money::new(dec!(1000000), Currency::INR),
        start: day(230),
        end: day(599),
        strategy: StrategyVersionInfo {
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
            slippage: SlippageAssumption::new("slip-v1", dec!(0.10)).unwrap(),
        },
        evidence: EvidencePolicy::ResearchPrior,
        slippage_ticks: dec!(1),
        close_time_utc: NaiveTime::from_hms_opt(10, 0, 0).unwrap(),
        calendar_version: "test".to_owned(),
        plan_slippage_ticks: None,
    }
}

fn risk() -> RiskConfig {
    let base = RiskConfig::new(toml::from_str::<RiskConfigData>(RISK).unwrap()).unwrap();
    research_risk_config(&base).unwrap()
}

fn costs() -> ScheduleCostModel {
    ScheduleCostModel::new(toml::from_str::<CostScheduleSet>(COSTS).unwrap()).unwrap()
}

fn data() -> Vec<InstrumentData> {
    let spec = spec();
    let bars = trending_bars(600);
    let series = BarSeries::new(spec.id, bars.last().unwrap().date(), bars).unwrap();
    vec![InstrumentData {
        spec,
        product: ProductType::Delivery,
        series,
    }]
}

#[tokio::test]
async fn research_stage_versions_cannot_trade_even_in_backtests() {
    // Research is not a trading stage; the Risk Gate refuses every entry.
    let report = run_backtest(
        &TrendPullback::v1(),
        &data(),
        &config(),
        &risk(),
        &costs(),
        &NoEvidence,
    )
    .await
    .unwrap();
    assert!(report.trades.is_empty());
    assert!(
        report.decisions.contains_key("risk_limit"),
        "{:?}",
        report.decisions
    );
}

fn paper_config() -> BacktestConfig {
    let mut c = config();
    c.strategy.stage = StrategyStage::Paper;
    c
}

#[tokio::test]
async fn a_full_backtest_trades_charges_costs_and_stays_consistent() {
    let report = run_backtest(
        &TrendPullback::v1(),
        &data(),
        &paper_config(),
        &risk(),
        &costs(),
        &NoEvidence,
    )
    .await
    .unwrap();
    assert!(
        !report.trades.is_empty(),
        "decisions: {:?}",
        report.decisions
    );
    assert_eq!(report.equity_curve.len(), 370);
    assert_eq!(report.reconciliation_mismatches, 0);
    assert_eq!(report.unprotected_events, 0);
    for trade in &report.trades {
        assert!(trade.costs > Decimal::ZERO);
        assert_eq!(trade.net_pnl, trade.gross_pnl - trade.costs);
        assert!(trade.closed_on >= trade.opened_on);
        // A stop never loses more than about 1R plus slippage and costs.
        assert!(trade.r_multiple > dec!(-1.5), "{trade:?}");
    }
    let m = &report.metrics;
    assert_eq!(m.trades as usize, report.trades.len());
    assert!(m.max_drawdown >= Decimal::ZERO && m.max_drawdown < dec!(0.2));
    let final_equity = report.equity_curve.last().unwrap().equity;
    let open_pnl = final_equity - dec!(1000000) - m.net_pnl;
    // Final equity = initial + realized net + open P&L of any open position.
    if report.open_positions == 0 {
        assert_eq!(open_pnl, Decimal::ZERO);
    }
}

#[tokio::test]
async fn backtests_are_deterministic() {
    let a = run_backtest(
        &TrendPullback::v1(),
        &data(),
        &paper_config(),
        &risk(),
        &costs(),
        &NoEvidence,
    )
    .await
    .unwrap();
    let b = run_backtest(
        &TrendPullback::v1(),
        &data(),
        &paper_config(),
        &risk(),
        &costs(),
        &NoEvidence,
    )
    .await
    .unwrap();
    // Ids are UUIDv7 with random bits; everything else must match exactly.
    let without_ids = |trades: &[qd_backtest::metrics::TradeRecord]| {
        trades
            .iter()
            .map(|t| {
                let mut t = t.clone();
                t.position = qd_domain::ids::PositionId::from_uuid(uuid::Uuid::nil());
                t.decision = None;
                t
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(without_ids(&a.trades), without_ids(&b.trades));
    assert_eq!(a.equity_curve, b.equity_curve);
    assert_eq!(a.decisions, b.decisions);
}

// ---------- fill rules ----------

fn bar(date: NaiveDate, open: Decimal, high: Decimal, low: Decimal, close: Decimal) -> Bar {
    Bar::new(BarData {
        date,
        open,
        high,
        low,
        close,
        volume: dec!(1),
    })
    .unwrap()
}

fn terms(order_type: OrderType, limit: Option<Decimal>, trigger: Option<Decimal>) -> OrderTerms {
    OrderTerms {
        order_type,
        limit: limit.map(|p| Price::new(p).unwrap()),
        trigger: trigger.map(|p| Price::new(p).unwrap()),
        validity: Validity::Day,
        product: ProductType::Delivery,
    }
}

fn fill(action: TradeAction, t: OrderTerms, b: &Bar) -> Option<Decimal> {
    simulate_fill(&spec(), action, t, b, false, dec!(1)).map(Price::value)
}

#[test]
fn stops_fill_at_the_open_on_a_gap_and_at_the_trigger_otherwise() {
    let stop = terms(OrderType::StopMarket, None, Some(dec!(95)));
    // Gap below the stop: filled at the open minus one tick, not at the stop.
    let gap = bar(day(1), dec!(92), dec!(93), dec!(90), dec!(91));
    assert_eq!(fill(TradeAction::CloseLong, stop, &gap), Some(dec!(91.95)));
    // Trades through the stop intraday: the stop minus one tick.
    let through = bar(day(1), dec!(97), dec!(98), dec!(94), dec!(96));
    assert_eq!(
        fill(TradeAction::CloseLong, stop, &through),
        Some(dec!(94.95))
    );
    // Never reaches it.
    let above = bar(day(1), dec!(97), dec!(98), dec!(96), dec!(97));
    assert_eq!(fill(TradeAction::CloseLong, stop, &above), None);
}

#[test]
fn stop_limit_entries_do_not_chase_gaps() {
    let entry = terms(OrderType::StopLimit, Some(dec!(100)), Some(dec!(100)));
    let gap_up = bar(day(1), dec!(101), dec!(103), dec!(100.5), dec!(102));
    assert_eq!(fill(TradeAction::OpenLong, entry, &gap_up), None);
    let triggers = bar(day(1), dec!(99), dec!(101), dec!(98), dec!(100.5));
    assert_eq!(
        fill(TradeAction::OpenLong, entry, &triggers),
        Some(dec!(100))
    );
}

#[test]
fn limits_fill_at_the_better_open_or_at_the_limit() {
    let target = terms(OrderType::Limit, Some(dec!(110)), None);
    let gap_up = bar(day(1), dec!(112), dec!(113), dec!(111), dec!(112));
    assert_eq!(
        fill(TradeAction::CloseLong, target, &gap_up),
        Some(dec!(112))
    );
    let reaches = bar(day(1), dec!(108), dec!(110.5), dec!(107), dec!(109));
    assert_eq!(
        fill(TradeAction::CloseLong, target, &reaches),
        Some(dec!(110))
    );
}

#[test]
fn same_bar_checks_only_protective_stops_at_their_trigger() {
    let stop = terms(OrderType::StopMarket, None, Some(dec!(95)));
    let b = bar(day(1), dec!(92), dec!(101), dec!(90), dec!(96));
    // On the entry bar the open came before the entry, so a gap fill at the open is impossible.
    assert_eq!(
        simulate_fill(&spec(), TradeAction::CloseLong, stop, &b, true, dec!(1)).map(Price::value),
        Some(dec!(94.95))
    );
    let target = terms(OrderType::Limit, Some(dec!(100)), None);
    assert_eq!(
        simulate_fill(&spec(), TradeAction::CloseLong, target, &b, true, dec!(1)),
        None
    );
}

#[tokio::test]
async fn when_a_bar_hits_both_stop_and_target_only_the_stop_fills() {
    // Every 7th bar from day 300 spans far beyond any stop and target.
    let spec = spec();
    let bars: Vec<Bar> = trending_bars(600)
        .into_iter()
        .enumerate()
        .map(|(n, b)| {
            if n >= 300 && n % 7 == 0 {
                bar(
                    b.date(),
                    b.open().value(),
                    b.close().value() + dec!(40),
                    b.close().value() - dec!(40),
                    b.close().value(),
                )
            } else {
                b
            }
        })
        .collect();
    let series = BarSeries::new(spec.id, bars.last().unwrap().date(), bars).unwrap();
    let data = vec![InstrumentData {
        spec,
        product: ProductType::Delivery,
        series,
    }];
    let report = run_backtest(
        &TrendPullback::v1(),
        &data,
        &paper_config(),
        &risk(),
        &costs(),
        &NoEvidence,
    )
    .await
    .unwrap();
    let wide = |d: NaiveDate| (d - day(0)).num_days() >= 300 && (d - day(0)).num_days() % 7 == 0;
    let on_wide: Vec<_> = report.trades.iter().filter(|t| wide(t.closed_on)).collect();
    assert!(!on_wide.is_empty(), "no trade was open on a wide bar");
    for trade in on_wide {
        assert_eq!(
            trade.exit_reason,
            qd_domain::outcome::ExitReason::StopHit,
            "{trade:?}"
        );
    }
    assert_eq!(report.reconciliation_mismatches, 0);
}
