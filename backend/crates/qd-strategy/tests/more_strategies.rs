//! Breakout, mean reversion and the short-side trend pullback on hand-made
//! feature sets (test data, not market data).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use chrono::{NaiveDate, TimeZone, Utc};
use qd_domain::action::EntryAction;
use qd_domain::ids::InstrumentId;
use qd_domain::instrument::{
    AssetClass, CalendarId, Capabilities, CorrelationBucket, InstrumentKind, InstrumentSpec,
    InstrumentSpecData, OrderType, ProductType, ProtectionMode, Validity, Venue,
};
use qd_domain::market::{Bar, BarData, BarSeries};
use qd_domain::num::Currency;
use qd_domain::plan::{InvalidationRule, TradePlan};
use qd_strategy::breakout::Breakout;
use qd_strategy::catalog::catalog;
use qd_strategy::features::{FEATURE_SET_VERSION, FeatureSet};
use qd_strategy::mean_reversion::MeanReversion;
use qd_strategy::regime::Regime;
use qd_strategy::strategy::{Strategy, StrategyInput, StrategyOutput};
use qd_strategy::trend_pullback_short::TrendPullbackShort;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

fn spec(can_short: bool) -> InstrumentSpec {
    InstrumentSpec::new(InstrumentSpecData {
        id: InstrumentId::new_at(Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap()),
        version: 1,
        effective_from: NaiveDate::from_ymd_opt(2020, 1, 1).unwrap(),
        effective_to: None,
        symbol: "TEST".to_owned(),
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
            can_short_overnight: can_short,
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

/// A feature set with the given last bar and levels; ATR 2.
#[allow(clippy::too_many_arguments)]
fn features(
    high: Decimal,
    low: Decimal,
    close: Decimal,
    sma20: Decimal,
    sma50: Decimal,
    sma200: Decimal,
    prior_high20: Decimal,
    prior_low20: Decimal,
    roc20: Decimal,
) -> FeatureSet {
    FeatureSet {
        version: FEATURE_SET_VERSION,
        as_of: NaiveDate::from_ymd_opt(2026, 6, 1).unwrap(),
        open: close,
        high,
        low,
        close,
        sma20,
        sma50,
        sma200,
        sma200_prev: sma200,
        atr14: dec!(2),
        atr_pct: dec!(2) / close,
        prior_high20,
        prior_low20,
        roc20,
    }
}

fn series() -> BarSeries {
    let bar = Bar::new(BarData {
        date: NaiveDate::from_ymd_opt(2026, 6, 1).unwrap(),
        open: dec!(100),
        high: dec!(100),
        low: dec!(100),
        close: dec!(100),
        volume: dec!(1),
    })
    .unwrap();
    BarSeries::new(spec(false).id, bar.date(), vec![bar]).unwrap()
}

fn run(s: &dyn Strategy, spec: &InstrumentSpec, f: &FeatureSet, regime: Regime) -> StrategyOutput {
    s.evaluate(&StrategyInput {
        spec,
        series: &series(),
        features: f,
        regime,
    })
}

fn setup(out: StrategyOutput) -> qd_strategy::strategy::SetupCandidate {
    match out {
        StrategyOutput::Setup(c) => *c,
        other => panic!("expected a setup, got {other:?}"),
    }
}

#[test]
fn breakout_enters_above_the_breakout_bar_and_skips_extended_or_down_markets() {
    let s = spec(false);
    // Close 101 over a prior high of 100 (0.5 ATR), above the 50-day average.
    let f = features(
        dec!(102),
        dec!(99),
        dec!(101),
        dec!(97),
        dec!(95),
        dec!(90),
        dec!(100),
        dec!(92),
        dec!(0.05),
    );
    let c = setup(run(&Breakout::v1(), &s, &f, Regime::TrendUp));
    assert_eq!(c.plan.action, EntryAction::OpenLong);
    assert_eq!(c.plan.entry, dec!(102.05));
    assert_eq!(c.plan.stop, dec!(98.05)); // 2 ATR
    assert_eq!(c.plan.target, dec!(114.05)); // 3R
    assert_eq!(
        c.plan.invalidation,
        vec![InvalidationRule::CloseBeyond {
            level: qd_domain::num::Price::new(dec!(97)).unwrap()
        }]
    );
    assert!(TradePlan::new(c.plan.clone(), &s).is_ok());
    // More than 1 ATR above the prior high: not chased.
    let extended = features(
        dec!(104),
        dec!(101),
        dec!(103),
        dec!(97),
        dec!(95),
        dec!(90),
        dec!(100),
        dec!(92),
        dec!(0.05),
    );
    assert_eq!(
        run(&Breakout::v1(), &s, &extended, Regime::TrendUp),
        StrategyOutput::NoSetup
    );
    assert!(matches!(
        run(&Breakout::v1(), &s, &f, Regime::TrendDown),
        StrategyOutput::Inactive { .. }
    ));
    // In a range it trades, and says why that is weaker.
    let c = setup(run(&Breakout::v1(), &s, &f, Regime::Range));
    assert!(c.strongest_argument_against.contains("Range"));
}

#[test]
fn mean_reversion_targets_the_average_only_inside_a_range() {
    let s = spec(false);
    // Close 96 is 2 ATR under the 20-day average (100), above the prior low (93).
    let f = features(
        dec!(97),
        dec!(95),
        dec!(96),
        dec!(100),
        dec!(100),
        dec!(100),
        dec!(104),
        dec!(93),
        dec!(-0.02),
    );
    let c = setup(run(&MeanReversion::v1(), &s, &f, Regime::Range));
    assert_eq!(c.plan.entry, dec!(97.05));
    assert_eq!(c.plan.stop, dec!(95.05)); // 1 ATR
    assert_eq!(c.plan.target, dec!(100)); // the 20-day average
    assert!(TradePlan::new(c.plan.clone(), &s).is_ok());
    assert!(matches!(
        run(&MeanReversion::v1(), &s, &f, Regime::TrendUp),
        StrategyOutput::Inactive { .. }
    ));
    // A new 20-day low is a breakdown, not a stretch.
    let breakdown = features(
        dec!(93),
        dec!(91),
        dec!(92),
        dec!(100),
        dec!(100),
        dec!(100),
        dec!(104),
        dec!(93),
        dec!(-0.05),
    );
    assert_eq!(
        run(&MeanReversion::v1(), &s, &breakdown, Regime::Range),
        StrategyOutput::NoSetup
    );
    // Not stretched enough.
    let mild = features(
        dec!(99.5),
        dec!(98),
        dec!(99),
        dec!(100),
        dec!(100),
        dec!(100),
        dec!(104),
        dec!(93),
        dec!(0),
    );
    assert_eq!(
        run(&MeanReversion::v1(), &s, &mild, Regime::Range),
        StrategyOutput::NoSetup
    );
}

#[test]
fn the_short_side_mirrors_the_long_one_and_only_where_a_short_can_be_held() {
    let futures = spec(true);
    // Rally high 99.5 within 0.5 ATR of the 20-day average (100), close below it.
    let f = features(
        dec!(99.5),
        dec!(97),
        dec!(98),
        dec!(100),
        dec!(103),
        dec!(110),
        dec!(106),
        dec!(95),
        dec!(-0.04),
    );
    let c = setup(run(
        &TrendPullbackShort::v1(),
        &futures,
        &f,
        Regime::TrendDown,
    ));
    assert_eq!(c.plan.action, EntryAction::OpenShort);
    assert_eq!(c.plan.entry, dec!(96.95));
    assert_eq!(c.plan.stop, dec!(100.95));
    assert_eq!(c.plan.target, dec!(86.95)); // 2.5R below
    assert!(TradePlan::new(c.plan.clone(), &futures).is_ok());
    // Cash equity cannot be shorted overnight: no setup, so no NO TRADE noise.
    assert_eq!(
        run(
            &TrendPullbackShort::v1(),
            &spec(false),
            &f,
            Regime::TrendDown
        ),
        StrategyOutput::NoSetup
    );
    assert!(matches!(
        run(&TrendPullbackShort::v1(), &futures, &f, Regime::TrendUp),
        StrategyOutput::Inactive { .. }
    ));
}

#[test]
fn the_catalog_has_every_strategy_once_with_its_parameters_and_floor() {
    let entries = catalog().unwrap();
    let versions: Vec<&str> = entries.iter().map(|e| e.strategy.logic_version()).collect();
    assert_eq!(
        versions,
        vec![
            "trend-pullback-1.0.0",
            "trend-pullback-short-1.0.0",
            "breakout-1.0.0",
            "mean-reversion-1.0.0"
        ]
    );
    for e in &entries {
        assert_eq!(
            e.parameters["rr_floor"],
            serde_json::json!(e.rr_floor.to_string())
        );
    }
}
