//! Feature engine, regime classifier and the trend-pullback strategy on
//! deterministic synthetic series (test data, not market data).

// Test code: a failed unwrap is a failed test. The workspace-wide unwrap/expect
// ban targets runtime code, and clippy's allow-unwrap-in-tests does not reach
// helper functions in integration-test files.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use chrono::{Days, NaiveDate, TimeZone, Utc};
use proptest::prelude::*;
use qd_domain::action::EntryAction;
use qd_domain::ids::InstrumentId;
use qd_domain::instrument::{
    AssetClass, CalendarId, Capabilities, CorrelationBucket, InstrumentKind, InstrumentSpec,
    InstrumentSpecData, OrderType, ProductType, ProtectionMode, Validity, Venue,
};
use qd_domain::market::{Bar, BarData, BarSeries};
use qd_domain::num::Currency;
use qd_domain::plan::{EntryOrderType, TradePlan};
use qd_domain::proposal::Grade;
use qd_strategy::features::{
    FeatureError, FeatureSet, MIN_BARS, atr, highest_high, rate_of_change, sma, true_ranges,
};
use qd_strategy::regime::{Regime, RegimeClassifier};
use qd_strategy::strategy::{StrategyOutput, run_strategy};
use qd_strategy::trend_pullback::TrendPullback;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

fn spec() -> InstrumentSpec {
    InstrumentSpec::new(InstrumentSpecData {
        id: InstrumentId::new_at(Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap()),
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

fn day(n: usize) -> NaiveDate {
    NaiveDate::from_ymd_opt(2025, 1, 1).unwrap() + Days::new(n as u64)
}

/// A bar around `close` with a fixed half-range.
fn bar(n: usize, close: Decimal, half_range: Decimal) -> Bar {
    Bar::new(BarData {
        date: day(n),
        open: close,
        high: close + half_range,
        low: close - half_range,
        close,
        volume: dec!(10000),
    })
    .unwrap()
}

/// Deterministic wiggle in [-1, 1] with period 10.
fn wiggle(n: usize) -> Decimal {
    [
        dec!(0),
        dec!(0.6),
        dec!(1),
        dec!(0.6),
        dec!(0),
        dec!(-0.6),
        dec!(-1),
        dec!(-0.6),
        dec!(0),
        dec!(0.3),
    ][n % 10]
}

fn series(bars: Vec<Bar>) -> BarSeries {
    let last = bars.last().unwrap().date();
    BarSeries::new(spec().id, last, bars).unwrap()
}

fn uptrend(len: usize) -> Vec<Bar> {
    (0..len)
        .map(|n| {
            bar(
                n,
                dec!(100) + dec!(0.25) * Decimal::from(n) + wiggle(n),
                dec!(0.5),
            )
        })
        .collect()
}

/// An uptrend whose last bar dips toward the 20-day average and closes above it.
fn uptrend_with_pullback(len: usize) -> Vec<Bar> {
    let mut bars = uptrend(len - 1);
    let previous = bars.last().unwrap().close().value();
    let close = previous - dec!(2);
    bars.push(
        Bar::new(BarData {
            date: day(len - 1),
            open: close + dec!(0.5),
            high: close + dec!(0.8),
            low: close - dec!(1.5),
            close,
            volume: dec!(10000),
        })
        .unwrap(),
    );
    bars
}

// ---------- features ----------

#[test]
fn indicator_worked_examples() {
    let values = [dec!(1), dec!(2), dec!(3), dec!(4), dec!(5)];
    assert_eq!(sma(&values, 5), Some(dec!(3)));
    assert_eq!(sma(&values, 2), Some(dec!(4.5)));
    assert_eq!(sma(&values, 6), None);
    assert_eq!(rate_of_change(&values, 4), Some(dec!(4)));

    let bars = vec![
        Bar::new(BarData {
            date: day(0),
            open: dec!(10),
            high: dec!(11),
            low: dec!(9),
            close: dec!(10),
            volume: dec!(1),
        })
        .unwrap(),
        // Gap up: true range uses the previous close.
        Bar::new(BarData {
            date: day(1),
            open: dec!(13),
            high: dec!(14),
            low: dec!(12.5),
            close: dec!(13),
            volume: dec!(1),
        })
        .unwrap(),
        Bar::new(BarData {
            date: day(2),
            open: dec!(13),
            high: dec!(13.5),
            low: dec!(12),
            close: dec!(12.5),
            volume: dec!(1),
        })
        .unwrap(),
    ];
    assert_eq!(true_ranges(&bars), Some(vec![dec!(2), dec!(4), dec!(1.5)]));
    // Wilder, period 2: first = (2 + 4) / 2 = 3, then (3 × 1 + 1.5) / 2 = 2.25.
    assert_eq!(atr(&bars, 2), Some(dec!(2.25)));
    assert_eq!(highest_high(&bars, 2), Some(dec!(14)));
}

#[test]
fn features_need_enough_history() {
    let s = series(uptrend(MIN_BARS - 1));
    assert_eq!(
        FeatureSet::compute(&s),
        Err(FeatureError::InsufficientHistory {
            needed: MIN_BARS,
            have: MIN_BARS - 1
        })
    );
    assert!(FeatureSet::compute(&series(uptrend(MIN_BARS))).is_ok());
}

// ---------- regime ----------

fn regime_of(bars: Vec<Bar>) -> Regime {
    RegimeClassifier::V1.classify(&FeatureSet::compute(&series(bars)).unwrap())
}

#[test]
fn regimes_are_classified_deterministically() {
    assert_eq!(regime_of(uptrend(260)), Regime::TrendUp);

    let down = (0..260)
        .map(|n| {
            bar(
                n,
                dec!(200) - dec!(0.25) * Decimal::from(n) + wiggle(n),
                dec!(0.5),
            )
        })
        .collect();
    assert_eq!(regime_of(down), Regime::TrendDown);

    let flat = (0..260).map(|n| bar(n, dec!(100), dec!(0.5))).collect();
    assert_eq!(regime_of(flat), Regime::Range);

    let wild = (0..260).map(|n| bar(n, dec!(100), dec!(5))).collect();
    assert_eq!(regime_of(wild), Regime::HighVolatility);
}

#[test]
fn a_falling_200_day_average_is_not_an_uptrend() {
    // A long decline followed by a slower recovery: price and the 50-day
    // average are back above the 200-day average, which is still falling.
    let bars: Vec<Bar> = (0..260)
        .map(|n| {
            let close = if n < 200 {
                dec!(200) - dec!(0.25) * Decimal::from(n)
            } else {
                dec!(150) + dec!(0.5) * Decimal::from(n - 200)
            };
            bar(n, close, dec!(0.5))
        })
        .collect();
    let f = FeatureSet::compute(&series(bars)).unwrap();
    assert!(
        f.close > f.sma200 && f.sma50 > f.sma200,
        "precondition: {f:?}"
    );
    assert!(f.sma200 < f.sma200_prev, "precondition: {f:?}");
    assert_eq!(RegimeClassifier::V1.classify(&f), Regime::Range);
}

// ---------- trend pullback ----------

#[test]
fn trend_pullback_proposes_a_valid_long_plan_after_a_pullback() {
    let spec = spec();
    let s = series(uptrend_with_pullback(260));
    let evaluation = run_strategy(&TrendPullback::v1(), &spec, &s, &RegimeClassifier::V1).unwrap();
    assert_eq!(evaluation.regime, Regime::TrendUp);
    let StrategyOutput::Setup(candidate) = evaluation.output else {
        panic!("expected a setup, got {:?}", evaluation.output);
    };
    let f = &evaluation.features;
    let plan = &candidate.plan;
    assert_eq!(plan.action, EntryAction::OpenLong);
    assert_eq!(plan.entry_type, EntryOrderType::StopLimit);
    assert_eq!(plan.entry, f.high + dec!(0.05));
    assert_eq!(plan.stop, plan.entry - dec!(2) * f.atr14);
    assert_eq!(plan.target, plan.entry + dec!(2.5) * (dec!(2) * f.atr14));
    assert_eq!(plan.max_holding_days, 15);
    assert_eq!(candidate.setup_type, "pullback_in_uptrend");
    assert_eq!(candidate.grade, Grade::A);
    assert!(!candidate.reasons.is_empty());
    assert!(!candidate.strongest_argument_against.is_empty());
    // The raw plan is valid once rounded to ticks.
    TradePlan::new(plan.clone(), &spec).unwrap();
}

#[test]
fn trend_pullback_waits_without_a_pullback_and_sits_out_other_regimes() {
    let spec = spec();
    let strategy = TrendPullback::v1();
    let evaluation = run_strategy(
        &strategy,
        &spec,
        &series(uptrend(260)),
        &RegimeClassifier::V1,
    )
    .unwrap();
    assert_eq!(evaluation.output, StrategyOutput::NoSetup);

    let flat = series((0..260).map(|n| bar(n, dec!(100), dec!(0.5))).collect());
    let evaluation = run_strategy(&strategy, &spec, &flat, &RegimeClassifier::V1).unwrap();
    assert_eq!(
        evaluation.output,
        StrategyOutput::Inactive {
            regime: Regime::Range
        }
    );
}

#[test]
fn trend_pullback_needs_the_close_to_hold_above_the_20_day_average() {
    let spec = spec();
    let mut bars = uptrend(259);
    let close = bars.last().unwrap().close().value() - dec!(5);
    bars.push(
        Bar::new(BarData {
            date: day(259),
            open: close + dec!(1),
            high: close + dec!(1.2),
            low: close - dec!(0.5),
            close,
            volume: dec!(10000),
        })
        .unwrap(),
    );
    let evaluation = run_strategy(
        &TrendPullback::v1(),
        &spec,
        &series(bars),
        &RegimeClassifier::V1,
    )
    .unwrap();
    assert_eq!(evaluation.regime, Regime::TrendUp);
    assert!(
        evaluation.features.close < evaluation.features.sma20,
        "precondition"
    );
    assert_eq!(evaluation.output, StrategyOutput::NoSetup);
}

#[test]
fn invariant_09_evaluations_use_only_bars_up_to_the_decision_date() {
    let spec = spec();
    let strategy = TrendPullback::v1();
    let bars = uptrend_with_pullback(300);
    let full = BarSeries::new(spec.id, bars.last().unwrap().date(), bars).unwrap();
    for end in [230_usize, 250, 270, 299] {
        let decision_date = day(end);
        // The same history built independently, as a live system would have it.
        let bars = full.bars()[..=end].to_vec();
        let independent = BarSeries::new(spec.id, decision_date, bars).unwrap();
        let from_full = full.as_of(decision_date);
        assert_eq!(from_full, independent);
        let a = run_strategy(&strategy, &spec, &from_full, &RegimeClassifier::V1).unwrap();
        let b = run_strategy(&strategy, &spec, &independent, &RegimeClassifier::V1).unwrap();
        assert_eq!(a, b, "evaluation at {decision_date} depends on later bars");
        assert_eq!(a.features.as_of, decision_date);
    }
}

#[test]
fn evaluations_are_deterministic() {
    let spec = spec();
    let s = series(uptrend_with_pullback(260));
    let a = run_strategy(&TrendPullback::v1(), &spec, &s, &RegimeClassifier::V1).unwrap();
    let b = run_strategy(&TrendPullback::v1(), &spec, &s, &RegimeClassifier::V1).unwrap();
    assert_eq!(a, b);
}

proptest! {
    #[test]
    fn any_valid_series_evaluates_without_panicking(
        steps in proptest::collection::vec(-300_i64..=300, MIN_BARS..MIN_BARS + 40),
        half_range in 1_i64..=500,
    ) {
        let mut close = dec!(1000);
        let mut bars = Vec::new();
        for (n, step) in steps.iter().enumerate() {
            close = (close + Decimal::new(*step, 2)).max(dec!(10));
            bars.push(bar(n, close, Decimal::new(half_range, 2)));
        }
        let spec = spec();
        let evaluation = run_strategy(&TrendPullback::v1(), &spec, &series(bars), &RegimeClassifier::V1)
            .unwrap();
        if let StrategyOutput::Setup(candidate) = evaluation.output {
            prop_assert!(candidate.plan.stop < candidate.plan.entry);
            prop_assert!(candidate.plan.entry < candidate.plan.target);
        }
    }
}
