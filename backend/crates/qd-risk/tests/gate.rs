//! Risk Gate: approvals, every rejection path, and the invariants it enforces.

// Test code: a failed unwrap is a failed test. The workspace-wide unwrap/expect
// ban targets runtime code, and clippy's allow-unwrap-in-tests does not reach
// helper functions in integration-test files.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use chrono::Duration;
use proptest::prelude::*;
use qd_domain::halt::{Halt, HaltBlock, HaltKind, HaltScope, HaltState};
use qd_domain::ids::HaltId;
use qd_domain::lifecycle::strategy::{StrategyStage, TradingStage};
use qd_domain::num::{Currency, FxRate, Money, Quantity};
use qd_domain::outcome::{InputKind, NoTradeReason, RiskLimitBreach};
use qd_domain::proposal::{AccountMode, TradeProposal};
use qd_risk::gate::{ApprovedEntry, RiskGate, RiskVerdict};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use common::*;

fn paper_proposal(spec: &qd_domain::instrument::InstrumentSpec) -> TradeProposal {
    proposal_with(
        spec,
        strategy_ref(),
        AccountMode::Paper,
        long_plan(),
        dec!(100),
    )
}

fn approved(verdict: RiskVerdict) -> ApprovedEntry {
    match verdict {
        RiskVerdict::Approved(entry) => *entry,
        RiskVerdict::Rejected(reason) => panic!("expected approval, got {reason:?}"),
    }
}

fn rejected(verdict: RiskVerdict) -> NoTradeReason {
    match verdict {
        RiskVerdict::Rejected(reason) => reason,
        RiskVerdict::Approved(entry) => panic!("expected rejection, got {entry:?}"),
    }
}

// ---------- approvals ----------

#[test]
fn paper_entry_is_sized_within_the_risk_budget_at_final_costs() {
    let (config, costs, spec) = (config(), costs(), equity_spec());
    let proposal = paper_proposal(&spec);
    let gate = RiskGate::new(&config, &costs);
    let entry = approved(gate.evaluate(
        &request(&proposal, &spec, StrategyStage::Paper),
        &account(AccountMode::Paper),
    ));
    // Paper accounts size as if at Full: 10,00,000 × 0.5% = 5,000.
    assert_eq!(entry.stage_multiplier, Decimal::ONE);
    assert_eq!(entry.risk_budget, inr(dec!(5000)));
    // Proposal risk_net at 100 shares is 5.1891 → 963 shares; costs at 963 are
    // lower per share, so the size is stable.
    assert_eq!(entry.quantity, Quantity::new(dec!(963)).unwrap());
    assert!(entry.planned_risk.amount <= dec!(5000));
    assert_eq!(entry.costs.reference_quantity(), entry.quantity);
    assert_eq!(entry.costs.model_version(), "zerodha-nse-equity-delivery@1");
    assert!(!entry.costs_verified);
    assert!(entry.economics.rr_net() >= dec!(2.0));
    assert!(entry.expected_value.in_r() >= dec!(0.10));
}

#[test]
fn live_small_capital_entry_uses_the_stage_multiplier_and_verified_costs() {
    let (config, costs, spec) = (config(), verified_costs(), equity_spec());
    let proposal = proposal_with(
        &spec,
        strategy_ref(),
        AccountMode::Live,
        long_plan(),
        dec!(100),
    );
    let gate = RiskGate::new(&config, &costs);
    let entry = approved(gate.evaluate(
        &request(&proposal, &spec, StrategyStage::SmallCapital),
        &account(AccountMode::Live),
    ));
    assert_eq!(entry.stage_multiplier, dec!(0.25));
    assert_eq!(entry.risk_budget, inr(dec!(1250)));
    assert!(entry.planned_risk.amount <= dec!(1250));
    assert!(entry.costs_verified);
}

#[test]
fn caps_shrink_the_size_to_the_remaining_headroom() {
    let (config, costs, spec) = (config(), costs(), equity_spec());
    let proposal = paper_proposal(&spec);
    let gate = RiskGate::new(&config, &costs);
    let mut state = account(AccountMode::Paper);
    // 4.8% already open elsewhere: 2,000 of the 5% cap is left.
    state.open_risk = vec![risk_item("other", None, dec!(48000))];
    let entry = approved(gate.evaluate(&request(&proposal, &spec, StrategyStage::Paper), &state));
    assert!(entry.quantity < Quantity::new(dec!(963)).unwrap());
    assert!(entry.planned_risk.amount <= dec!(2000));
    assert!(entry.planned_risk.amount > dec!(1900));
}

// ---------- rejections ----------

fn evaluate_with(state_change: impl FnOnce(&mut qd_risk::gate::AccountRiskState)) -> NoTradeReason {
    let (config, costs, spec) = (config(), costs(), equity_spec());
    let proposal = paper_proposal(&spec);
    let gate = RiskGate::new(&config, &costs);
    let mut state = account(AccountMode::Paper);
    state_change(&mut state);
    rejected(gate.evaluate(&request(&proposal, &spec, StrategyStage::Paper), &state))
}

#[test]
fn invariant_03_each_limit_can_reject_an_otherwise_good_entry() {
    // Daily loss: down 2.1% on the day.
    assert!(matches!(
        evaluate_with(|s| s.equity = inr(dec!(979000))),
        NoTradeReason::RiskLimit(RiskLimitBreach::DailyLoss { .. })
    ));
    // Weekly loss: flat today, down 4% on the week.
    assert!(matches!(
        evaluate_with(|s| {
            s.equity = inr(dec!(960000));
            s.equity_at_day_start = inr(dec!(960000));
        }),
        NoTradeReason::RiskLimit(RiskLimitBreach::WeeklyLoss { .. })
    ));
    // Drawdown: 10% below the high-water mark.
    assert!(matches!(
        evaluate_with(|s| s.high_water_mark = inr(dec!(1111112))),
        NoTradeReason::RiskLimit(RiskLimitBreach::Drawdown { .. })
    ));
    // Cool-off after 3 losing trades.
    assert_eq!(
        evaluate_with(|s| s.consecutive_losses = 3),
        NoTradeReason::RiskLimit(RiskLimitBreach::ConsecutiveLosses {
            losses: 3,
            limit: 3
        })
    );
    // Total open-risk cap with no headroom.
    assert!(matches!(
        evaluate_with(|s| s.open_risk = vec![risk_item("other", None, dec!(50000))]),
        NoTradeReason::RiskLimit(RiskLimitBreach::TotalOpenRisk { .. })
    ));
    // Correlated-bucket cap.
    assert!(matches!(
        evaluate_with(|s| s.open_risk = vec![risk_item("india_equity", None, dec!(20000))]),
        NoTradeReason::RiskLimit(RiskLimitBreach::CorrelatedBucket { .. })
    ));
    // Strategy-version cap.
    let version = strategy_ref().version_id;
    assert!(matches!(
        evaluate_with(|s| s.open_risk = vec![risk_item("other", Some(version), dec!(20000))]),
        NoTradeReason::RiskLimit(RiskLimitBreach::StrategyVersion { .. })
    ));
    // Too small: 1,000 INR of equity risks 5 INR, less than one share's risk.
    assert!(matches!(
        evaluate_with(|s| {
            for m in [
                &mut s.equity,
                &mut s.equity_at_day_start,
                &mut s.equity_at_week_start,
                &mut s.high_water_mark,
            ] {
                *m = inr(dec!(1000));
            }
        }),
        NoTradeReason::PositionTooSmall { .. }
    ));
}

#[test]
fn invariant_02_active_halts_reject_entries() {
    let start = at(9, 0);
    let halt = Halt::new(
        HaltId::new_at(start),
        HaltKind::Manual,
        HaltScope::Global,
        "owner paused trading",
        start,
        None,
        true,
    )
    .unwrap();
    let id = halt.id();
    assert_eq!(
        evaluate_with(|s| s.halts = HaltState::Known(vec![halt])),
        NoTradeReason::KillSwitch(HaltBlock::Active {
            halt: id,
            kind: HaltKind::Manual,
            scope: HaltScope::Global
        })
    );
}

#[test]
fn invariant_06_unknown_or_inconsistent_inputs_fail_closed() {
    assert_eq!(
        evaluate_with(|s| s.halts = HaltState::Unknown),
        NoTradeReason::KillSwitch(HaltBlock::StateUnknown)
    );
    assert_eq!(
        evaluate_with(|s| s.equity = Money::new(dec!(1000000), Currency::USD)),
        NoTradeReason::MissingOrInconsistentData {
            input: InputKind::Equity
        }
    );
    assert_eq!(
        evaluate_with(|s| s.equity = inr(Decimal::ZERO)),
        NoTradeReason::MissingOrInconsistentData {
            input: InputKind::Equity
        }
    );
    // Equity above the high-water mark: the mark is stale.
    assert_eq!(
        evaluate_with(|s| s.equity = inr(dec!(1000001))),
        NoTradeReason::MissingOrInconsistentData {
            input: InputKind::Equity
        }
    );
    assert_eq!(
        evaluate_with(|s| s.open_risk = vec![qd_risk::gate::RiskItem {
            amount: Money::new(dec!(1), Currency::USD),
            ..risk_item("x", None, dec!(1))
        }]),
        NoTradeReason::MissingOrInconsistentData {
            input: InputKind::Positions
        }
    );
    // A mode mismatch between proposal and account.
    assert_eq!(
        evaluate_with(|s| s.mode = AccountMode::Backtest),
        NoTradeReason::MissingOrInconsistentData {
            input: InputKind::Configuration
        }
    );

    // An FX rate observed after the decision time.
    let (config, costs, spec) = (config(), costs(), equity_spec());
    let proposal = paper_proposal(&spec);
    let gate = RiskGate::new(&config, &costs);
    let mut req = request(&proposal, &spec, StrategyStage::Paper);
    req.fx = FxRate::identity(Currency::INR, req.at + Duration::seconds(1));
    assert_eq!(
        rejected(gate.evaluate(&req, &account(AccountMode::Paper))),
        NoTradeReason::MissingOrInconsistentData {
            input: InputKind::FxRates
        }
    );
    // A decision time before the proposal's as-of time.
    let mut req = request(&proposal, &spec, StrategyStage::Paper);
    req.at = proposal.as_of() - Duration::seconds(1);
    req.fx = FxRate::identity(Currency::INR, req.at);
    assert_eq!(
        rejected(gate.evaluate(&req, &account(AccountMode::Paper))),
        NoTradeReason::MissingOrInconsistentData {
            input: InputKind::Configuration
        }
    );
}

#[test]
fn invariant_10_only_trading_stages_pass() {
    let (config, costs, spec) = (config(), costs(), equity_spec());
    let proposal = paper_proposal(&spec);
    let gate = RiskGate::new(&config, &costs);
    for stage in [
        StrategyStage::Draft,
        StrategyStage::Research,
        StrategyStage::ResearchPassed,
        StrategyStage::Rejected,
        StrategyStage::Retired,
        StrategyStage::Suspended {
            resume_to: TradingStage::Full,
        },
    ] {
        assert_eq!(
            rejected(gate.evaluate(
                &request(&proposal, &spec, stage),
                &account(AccountMode::Paper)
            )),
            NoTradeReason::RiskLimit(RiskLimitBreach::StageNotEligible { stage })
        );
    }
}

#[test]
fn invariant_14_live_needs_a_live_stage_and_verified_costs() {
    let config = config();
    let spec = equity_spec();
    let proposal = proposal_with(
        &spec,
        strategy_ref(),
        AccountMode::Live,
        long_plan(),
        dec!(100),
    );
    let live = account(AccountMode::Live);

    let verified = verified_costs();
    let gate = RiskGate::new(&config, &verified);
    assert_eq!(
        rejected(gate.evaluate(&request(&proposal, &spec, StrategyStage::Paper), &live)),
        NoTradeReason::RiskLimit(RiskLimitBreach::StageNotEligible {
            stage: StrategyStage::Paper
        })
    );

    let unverified = costs();
    let gate = RiskGate::new(&config, &unverified);
    assert_eq!(
        rejected(gate.evaluate(&request(&proposal, &spec, StrategyStage::Full), &live)),
        NoTradeReason::MissingOrInconsistentData {
            input: InputKind::Configuration
        }
    );
}

#[test]
fn shorts_need_an_instrument_that_allows_overnight_shorts() {
    let (config, costs, spec) = (config(), costs(), equity_spec());
    let proposal = proposal_with(
        &spec,
        strategy_ref(),
        AccountMode::Paper,
        short_plan(),
        dec!(100),
    );
    let gate = RiskGate::new(&config, &costs);
    assert_eq!(
        rejected(gate.evaluate(
            &request(&proposal, &spec, StrategyStage::Paper),
            &account(AccountMode::Paper)
        )),
        NoTradeReason::ShortNotPermitted
    );
}

#[test]
fn proposal_gates_reject_low_rr_and_low_ev() {
    let (config, costs, spec) = (config(), costs(), equity_spec());
    let proposal = paper_proposal(&spec);
    let gate = RiskGate::new(&config, &costs);
    let mut req = request(&proposal, &spec, StrategyStage::Paper);
    req.rr_floor = dec!(3);
    assert!(matches!(
        rejected(gate.evaluate(&req, &account(AccountMode::Paper))),
        NoTradeReason::RiskRewardBelowFloor { .. }
    ));

    let mut strict = qd_risk::config::RiskConfigData::clone(&config);
    strict.min_ev_r = dec!(0.6);
    let strict = qd_risk::config::RiskConfig::new(strict).unwrap();
    let gate = RiskGate::new(&strict, &costs);
    assert!(matches!(
        rejected(gate.evaluate(
            &request(&proposal, &spec, StrategyStage::Paper),
            &account(AccountMode::Paper)
        )),
        NoTradeReason::InsufficientEdge { .. }
    ));
}

#[test]
fn fixed_fees_can_make_a_small_account_uneconomic() {
    // 20,000 INR risks 100 per trade → about 16 shares, where the flat DP
    // charge pushes net RR below the 2.0 floor.
    assert_eq!(
        evaluate_with(|s| {
            for m in [
                &mut s.equity,
                &mut s.equity_at_day_start,
                &mut s.equity_at_week_start,
                &mut s.high_water_mark,
            ] {
                *m = inr(dec!(20000));
            }
        }),
        NoTradeReason::UneconomicAfterCosts
    );
}

#[test]
fn size_shrinks_when_per_unit_costs_rise_at_small_sizes() {
    // 60,000 INR risks 300. At the proposal's per-share risk that is 57 shares,
    // but the flat DP charge makes each share riskier at 57 than at 100, so
    // the gate must size down until the final planned risk fits the budget.
    let (config, costs, spec) = (config(), costs(), equity_spec());
    let proposal = paper_proposal(&spec);
    let gate = RiskGate::new(&config, &costs);
    let mut state = account(AccountMode::Paper);
    for m in [
        &mut state.equity,
        &mut state.equity_at_day_start,
        &mut state.equity_at_week_start,
        &mut state.high_water_mark,
    ] {
        *m = inr(dec!(60000));
    }
    let entry = approved(gate.evaluate(&request(&proposal, &spec, StrategyStage::Paper), &state));
    assert!(entry.quantity < Quantity::new(dec!(57)).unwrap());
    assert!(entry.planned_risk.amount <= dec!(300));
    assert!(entry.economics.risk_net() > proposal.economics().risk_net());
}

proptest! {
    #[test]
    fn approvals_never_exceed_the_budget_or_the_caps(
        equity in 20_000_i64..=50_000_000,
        other_open in 0_i64..=60,
        bucket_open in 0_i64..=25,
    ) {
        let (config, costs, spec) = (config(), costs(), equity_spec());
        let proposal = paper_proposal(&spec);
        let gate = RiskGate::new(&config, &costs);
        let equity = Decimal::from(equity);
        let mut state = account(AccountMode::Paper);
        state.equity = inr(equity);
        state.equity_at_day_start = inr(equity);
        state.equity_at_week_start = inr(equity);
        state.high_water_mark = inr(equity);
        // Open risk as tenths of a percent of equity.
        let other = equity * Decimal::from(other_open) / dec!(1000);
        let bucket = equity * Decimal::from(bucket_open) / dec!(1000);
        state.open_risk = vec![
            risk_item("other", None, other),
            risk_item("india_equity", None, bucket),
        ];
        if let RiskVerdict::Approved(entry) =
            gate.evaluate(&request(&proposal, &spec, StrategyStage::Paper), &state)
        {
            let planned = entry.planned_risk.amount;
            prop_assert!(planned <= equity * dec!(0.005));
            prop_assert!(other + bucket + planned <= equity * dec!(0.05));
            prop_assert!(bucket + planned <= equity * dec!(0.02));
            prop_assert!(spec.is_valid_order_quantity(entry.quantity));
            prop_assert!(entry.economics.rr_net() >= dec!(2.0));
        }
    }
}
