//! The pure decision pipeline end to end, as every mode will run it (INV-08):
//! completed bars → features → regime → strategy → complete proposal → Risk Gate.
//! The bars and the outcome probabilities are test data.

// Test code: a failed unwrap is a failed test. The workspace-wide unwrap/expect
// ban targets runtime code, and clippy's allow-unwrap-in-tests does not reach
// helper functions in integration-test files.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use chrono::{Days, NaiveDate};
use qd_domain::lifecycle::strategy::StrategyStage;
use qd_domain::market::{Bar, BarData, BarSeries};
use qd_domain::proposal::AccountMode;
use qd_risk::gate::{RiskGate, RiskVerdict};
use qd_strategy::regime::RegimeClassifier;
use qd_strategy::strategy::{Strategy, StrategyOutput, run_strategy};
use qd_strategy::trend_pullback::TrendPullback;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use common::*;

fn day(n: usize) -> NaiveDate {
    date() - Days::new(300) + Days::new(n as u64)
}

/// A steady uptrend whose last completed bar pulls back toward the 20-day average.
fn bars() -> Vec<Bar> {
    let wiggle = [0, 6, 10, 6, 0, -6, -10, -6, 0, 3];
    let mut bars: Vec<Bar> = (0..259)
        .map(|n| {
            let close = dec!(100) + dec!(0.25) * Decimal::from(n) + Decimal::new(wiggle[n % 10], 1);
            Bar::new(BarData {
                date: day(n),
                open: close,
                high: close + dec!(0.5),
                low: close - dec!(0.5),
                close,
                volume: dec!(10000),
            })
            .unwrap()
        })
        .collect();
    let close = bars.last().unwrap().close().value() - dec!(2);
    bars.push(
        Bar::new(BarData {
            date: day(259),
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

#[test]
fn bars_to_an_approved_paper_entry() {
    let spec = equity_spec();
    let bars = bars();
    let series = BarSeries::new(spec.id, bars.last().unwrap().date(), bars).unwrap();
    let strategy = TrendPullback::v1();
    let evaluation = run_strategy(&strategy, &spec, &series, &RegimeClassifier::V1).unwrap();
    let StrategyOutput::Setup(candidate) = evaluation.output else {
        panic!("expected a setup, got {:?}", evaluation.output);
    };

    let mut strategy_ref = strategy_ref();
    strategy_ref.logic_version = strategy.logic_version().to_owned();
    let proposal = proposal_with(
        &spec,
        strategy_ref,
        AccountMode::Paper,
        candidate.plan,
        dec!(500),
    );
    // Levels are tick-aligned before the Risk Gate sees them.
    assert!(spec.is_tick_aligned(proposal.plan().entry().price));
    assert!(spec.is_tick_aligned(proposal.plan().stop()));

    let (config, costs) = (config(), costs());
    let gate = RiskGate::new(&config, &costs);
    let mut req = request(&proposal, &spec, StrategyStage::Paper);
    req.rr_floor = strategy.params().rr_floor;
    match gate.evaluate(&req, &account(AccountMode::Paper)) {
        RiskVerdict::Approved(entry) => {
            assert!(spec.is_valid_order_quantity(entry.quantity));
            assert!(entry.economics.rr_net() >= dec!(1.5));
            assert!(entry.planned_risk.amount <= dec!(5000));
        }
        RiskVerdict::Rejected(reason) => panic!("expected approval, got {reason:?}"),
    }
}
