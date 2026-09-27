//! Worked examples for the §6.5 formulas, instrument validation and proposals.

// Test code: a failed unwrap is a failed test. The workspace-wide unwrap/expect
// ban targets runtime code, and clippy's allow-unwrap-in-tests does not reach
// helper functions in integration-test files.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use qd_domain::action::{EntryAction, Side};
use qd_domain::economics::{
    CostEstimate, CostLine, ExpectedValue, OutcomeProbabilities, SlippageAssumption, UnitEconomics,
};
use qd_domain::ids::{ProposalId, SnapshotId, StrategyId, StrategyVersionId};
use qd_domain::instrument::{InstrumentError, InstrumentKind, InstrumentSpec, Rounding};
use qd_domain::num::{Currency, FxRate, Money, Price, Quantity, Ratio};
use qd_domain::plan::{EntryOrderType, InvalidationRule, PlanDefect, TradePlan, TradePlanInput};
use qd_domain::portfolio::{
    PortfolioError, PositionExposure, Protection, daily_pnl, drawdown, position_open_risk,
    r_multiple, total_open_risk, update_high_water_mark, working_entry_risk,
};
use qd_domain::proposal::{
    AccountMode, Explanation, FactorValue, Grade, ProposalDraft, ProposalError, Reason,
    ReasonDirection, Reproducibility, StrategyRef, TradeProposal,
};
use qd_domain::sizing::{SizingError, SizingInput, SizingOutcome, cap_quantity, size_position};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

fn plan_input(
    action: EntryAction,
    entry: Decimal,
    stop: Decimal,
    target: Decimal,
) -> TradePlanInput {
    TradePlanInput {
        action,
        entry_type: EntryOrderType::Limit,
        entry,
        stop,
        target,
        max_holding_days: 10,
        invalidation: vec![InvalidationRule::RegimeChange],
    }
}

fn costs(currency: Currency, reference_quantity: Decimal, total: Decimal) -> CostEstimate {
    CostEstimate::new(
        "cost-v1",
        currency,
        Quantity::new(reference_quantity).unwrap(),
        vec![
            CostLine {
                name: "brokerage".to_owned(),
                amount: total / dec!(2),
            },
            CostLine {
                name: "taxes and fees".to_owned(),
                amount: total / dec!(2),
            },
        ],
    )
    .unwrap()
}

fn money(amount: Decimal) -> Money {
    Money::new(amount, Currency::INR)
}

// ---------- instrument specs ----------

#[test]
fn instrument_spec_rejects_invalid_contract_terms() {
    let mut data = common::equity_data();
    data.tick_size = Decimal::ZERO;
    assert!(matches!(
        InstrumentSpec::new(data),
        Err(InstrumentError::NonPositiveTerm {
            field: "tick_size",
            ..
        })
    ));

    let mut data = common::equity_data();
    data.lot_size = dec!(75);
    data.quantity_step = dec!(50);
    assert!(matches!(
        InstrumentSpec::new(data),
        Err(InstrumentError::StepNotMultipleOfLot { .. })
    ));

    let mut data = common::equity_data();
    data.kind = InstrumentKind::Future;
    assert_eq!(
        InstrumentSpec::new(data),
        Err(InstrumentError::ExpiryMismatch)
    );

    let mut data = common::equity_data();
    data.capabilities.products.clear();
    assert_eq!(
        InstrumentSpec::new(data),
        Err(InstrumentError::EmptyCapabilityList("products"))
    );
}

#[test]
fn instrument_spec_effective_dates_are_half_open() {
    let mut data = common::equity_data();
    data.effective_to = Some(common::date());
    let spec = InstrumentSpec::new(data).unwrap();
    assert!(spec.is_effective_on(common::date().pred_opt().unwrap()));
    assert!(!spec.is_effective_on(common::date()));
}

#[test]
fn instrument_spec_round_trips_through_json_and_revalidates() {
    let spec = common::crude_future();
    let json = serde_json::to_string(&spec).unwrap();
    assert_eq!(serde_json::from_str::<InstrumentSpec>(&json).unwrap(), spec);
    let broken = json.replace(r#""tick_size":"1""#, r#""tick_size":"0""#);
    assert!(serde_json::from_str::<InstrumentSpec>(&broken).is_err());
}

#[test]
fn shorts_are_permitted_only_where_the_spec_allows_overnight_shorts() {
    assert!(common::equity().permits_overnight(Side::Long));
    assert!(!common::equity().permits_overnight(Side::Short));
    assert!(common::crude_future().permits_overnight(Side::Short));
}

#[test]
fn price_rounding_follows_the_requested_direction() {
    let spec = common::equity();
    assert_eq!(
        spec.round_price(dec!(100.02), Rounding::Up)
            .unwrap()
            .value(),
        dec!(100.05)
    );
    assert_eq!(
        spec.round_price(dec!(100.02), Rounding::Down)
            .unwrap()
            .value(),
        dec!(100.00)
    );
    assert_eq!(
        spec.round_price(dec!(100.05), Rounding::Up)
            .unwrap()
            .value(),
        dec!(100.05)
    );
    assert!(spec.round_price(dec!(0.02), Rounding::Down).is_err());
}

// ---------- trade plans ----------

#[test]
fn long_plan_rounds_conservatively() {
    let plan = TradePlan::new(
        plan_input(
            EntryAction::OpenLong,
            dec!(100.02),
            dec!(95.53),
            dec!(112.74),
        ),
        &common::equity(),
    )
    .unwrap();
    assert_eq!(plan.entry().price.value(), dec!(100.05)); // entry up
    assert_eq!(plan.stop().value(), dec!(95.50)); // stop down: more risk
    assert_eq!(plan.target().value(), dec!(112.70)); // target down: less reward
    assert_eq!(plan.risk_points(), dec!(4.55));
    assert_eq!(plan.reward_points(), dec!(12.65));
}

#[test]
fn short_plan_rounds_conservatively() {
    let plan = TradePlan::new(
        plan_input(
            EntryAction::OpenShort,
            dec!(6000.4),
            dec!(6119.2),
            dec!(5760.6),
        ),
        &common::crude_future(),
    )
    .unwrap();
    assert_eq!(plan.entry().price.value(), dec!(6000)); // entry down
    assert_eq!(plan.stop().value(), dec!(6120)); // stop up: more risk
    assert_eq!(plan.target().value(), dec!(5761)); // target up: less reward
}

#[test]
fn plan_geometry_is_validated_after_rounding() {
    let spec = common::equity();
    assert_eq!(
        TradePlan::new(
            plan_input(EntryAction::OpenLong, dec!(100), dec!(101), dec!(110)),
            &spec
        ),
        Err(PlanDefect::StopOnWrongSide)
    );
    assert_eq!(
        TradePlan::new(
            plan_input(EntryAction::OpenLong, dec!(100), dec!(95), dec!(99)),
            &spec
        ),
        Err(PlanDefect::TargetOnWrongSide)
    );
    // Target 100.04 rounds down to 100.00, onto the entry: no reward left.
    assert_eq!(
        TradePlan::new(
            plan_input(EntryAction::OpenLong, dec!(100), dec!(95), dec!(100.04)),
            &spec
        ),
        Err(PlanDefect::TargetOnWrongSide)
    );
    let mut zero_days = plan_input(EntryAction::OpenLong, dec!(100), dec!(95), dec!(110));
    zero_days.max_holding_days = 0;
    assert_eq!(
        TradePlan::new(zero_days, &spec),
        Err(PlanDefect::ZeroHoldingPeriod)
    );
}

// ---------- per-unit economics and EV ----------

#[test]
fn equity_long_economics_worked_example() {
    let spec = common::equity();
    let plan = TradePlan::new(
        plan_input(
            EntryAction::OpenLong,
            dec!(100.00),
            dec!(95.50),
            dec!(112.70),
        ),
        &spec,
    )
    .unwrap();
    // 20 INR round trip for 100 shares = 0.20 per share; 0.30 stop slippage.
    let economics = UnitEconomics::compute(
        &plan,
        &spec,
        &costs(Currency::INR, dec!(100), dec!(20)),
        &SlippageAssumption::new("slip-v1", dec!(0.30)).unwrap(),
    )
    .unwrap();
    assert_eq!(economics.risk_gross(), dec!(4.50));
    assert_eq!(economics.reward_gross(), dec!(12.70));
    assert_eq!(economics.costs_per_unit(), dec!(0.20));
    assert_eq!(economics.risk_net(), dec!(5.00)); // 4.50 + 0.20 + 0.30
    assert_eq!(economics.reward_net(), dec!(12.50)); // 12.70 - 0.20
    assert_eq!(economics.rr_net(), dec!(2.5));

    let probabilities =
        OutcomeProbabilities::new(dec!(0.40), dec!(0.45), dec!(0.15), "empirical-v1", 42).unwrap();
    let ev = ExpectedValue::compute(&economics, &probabilities, dec!(1.00)).unwrap();
    // 0.40 × 12.50 − 0.45 × 5.00 + 0.15 × 1.00 = 2.90
    assert_eq!(ev.per_unit(), dec!(2.90));
    assert_eq!(ev.in_r(), dec!(0.58));
}

#[test]
fn futures_economics_use_the_contract_multiplier() {
    let spec = common::crude_future();
    let plan = TradePlan::new(
        plan_input(EntryAction::OpenShort, dec!(6000), dec!(6120), dec!(5760)),
        &spec,
    )
    .unwrap();
    let economics = UnitEconomics::compute(
        &plan,
        &spec,
        &costs(Currency::INR, dec!(1), dec!(400)),
        &SlippageAssumption::new("slip-v1", dec!(10)).unwrap(),
    )
    .unwrap();
    // 120 points × 100 per point = 12,000 per lot; + 400 costs + 10 × 100 slippage.
    assert_eq!(economics.risk_gross(), dec!(12000));
    assert_eq!(economics.risk_net(), dec!(13400));
    assert_eq!(economics.reward_net(), dec!(23600));
    assert_eq!(economics.rr_net(), dec!(23600) / dec!(13400));
}

#[test]
fn costs_that_consume_the_reward_invalidate_the_plan() {
    let spec = common::equity();
    let plan = TradePlan::new(
        plan_input(EntryAction::OpenLong, dec!(100), dec!(99), dec!(100.50)),
        &spec,
    )
    .unwrap();
    let result = UnitEconomics::compute(
        &plan,
        &spec,
        &costs(Currency::INR, dec!(1), dec!(0.60)),
        &SlippageAssumption::new("slip-v1", Decimal::ZERO).unwrap(),
    );
    assert_eq!(result, Err(PlanDefect::NonPositiveNetReward));
}

#[test]
fn economics_reject_costs_in_another_currency() {
    let spec = common::equity();
    let plan = TradePlan::new(
        plan_input(EntryAction::OpenLong, dec!(100), dec!(95), dec!(110)),
        &spec,
    )
    .unwrap();
    let result = UnitEconomics::compute(
        &plan,
        &spec,
        &costs(Currency::USD, dec!(1), dec!(1)),
        &SlippageAssumption::new("slip-v1", Decimal::ZERO).unwrap(),
    );
    assert_eq!(result, Err(PlanDefect::CurrencyMismatch));
}

#[test]
fn probabilities_must_be_a_distribution() {
    assert!(OutcomeProbabilities::new(dec!(0.5), dec!(0.5), dec!(0.1), "m", 30).is_err());
    assert!(OutcomeProbabilities::new(dec!(1.1), dec!(-0.1), dec!(0), "m", 30).is_err());
    assert!(OutcomeProbabilities::new(dec!(0.5), dec!(0.5), dec!(0), "", 30).is_err());
    assert!(OutcomeProbabilities::new(dec!(0.5), dec!(0.3), dec!(0.2), "m", 30).is_ok());
}

#[test]
fn cost_estimates_are_validated() {
    let quantity = Quantity::new(dec!(10)).unwrap();
    let negative = vec![CostLine {
        name: "rebate".to_owned(),
        amount: dec!(-1),
    }];
    assert!(CostEstimate::new("v1", Currency::INR, quantity, negative).is_err());
    assert!(CostEstimate::new("", Currency::INR, quantity, vec![]).is_err());
    assert!(CostEstimate::new("v1", Currency::INR, Quantity::ZERO, vec![]).is_err());
    assert!(SlippageAssumption::new("v1", dec!(-0.01)).is_err());
}

// ---------- sizing ----------

fn sizing(equity: Decimal, risk_net_per_unit: Money, fx: FxRate) -> SizingInput {
    SizingInput {
        equity: money(equity),
        risk_per_trade: Ratio::from_percent(dec!(0.5)).unwrap(),
        stage_multiplier: Decimal::ONE,
        risk_net_per_unit,
        fx,
    }
}

fn inr_identity() -> FxRate {
    FxRate::identity(Currency::INR, common::at(9, 0))
}

#[test]
fn equity_sizing_worked_example() {
    // 10,00,000 × 0.5% = 5,000 at risk; 5.00 per share → 1,000 shares.
    let input = sizing(dec!(1000000), money(dec!(5.00)), inr_identity());
    let SizingOutcome::Sized(size) = size_position(&input, &common::equity()).unwrap() else {
        panic!("expected a sized position");
    };
    assert_eq!(size.qty_raw(), dec!(1000));
    assert_eq!(size.quantity().value(), dec!(1000));
    assert_eq!(size.planned_risk(), money(dec!(5000.00)));
    assert_eq!(size.risk_budget(), money(dec!(5000.00)));
}

#[test]
fn futures_sizing_counts_the_multiplier() {
    // One crude lot risks 13,400 INR. With a 5,000 INR budget the position is too small;
    // ignoring the multiplier would have sized 37 lots (5,000 / 134).
    let spec = common::crude_future();
    let input = sizing(dec!(1000000), money(dec!(13400)), inr_identity());
    assert_eq!(
        size_position(&input, &spec).unwrap(),
        SizingOutcome::TooSmall {
            quantity: Quantity::ZERO,
            min: Quantity::new(Decimal::ONE).unwrap(),
        }
    );

    let input = sizing(dec!(5000000), money(dec!(13400)), inr_identity());
    let SizingOutcome::Sized(size) = size_position(&input, &spec).unwrap() else {
        panic!("expected a sized position");
    };
    assert_eq!(size.quantity().value(), Decimal::ONE);
    assert_eq!(size.planned_risk(), money(dec!(13400)));
}

#[test]
fn crypto_sizing_converts_currency_and_rounds_to_the_step() {
    let spec = common::crypto_spot();
    let fx = FxRate::new(Currency::USDT, Currency::INR, dec!(85), common::at(9, 0)).unwrap();
    let input = sizing(dec!(1000000), Money::new(dec!(2000), Currency::USDT), fx);
    let SizingOutcome::Sized(size) = size_position(&input, &spec).unwrap() else {
        panic!("expected a sized position");
    };
    // 2,000 USDT × 85 = 170,000 INR per coin; 5,000 / 170,000 = 0.0294117… → 0.02941.
    assert_eq!(size.risk_per_unit(), money(dec!(170000)));
    assert_eq!(size.quantity().value(), dec!(0.02941));
    assert_eq!(size.planned_risk(), money(dec!(4999.70000)));
}

#[test]
fn sizing_fails_closed_on_bad_inputs() {
    let spec = common::equity();
    let mut input = sizing(Decimal::ZERO, money(dec!(5)), inr_identity());
    assert_eq!(
        size_position(&input, &spec),
        Err(SizingError::NonPositiveEquity)
    );

    input = sizing(dec!(1000000), money(dec!(5)), inr_identity());
    input.stage_multiplier = dec!(1.5);
    assert_eq!(
        size_position(&input, &spec),
        Err(SizingError::InvalidStageMultiplier(dec!(1.5)))
    );

    let usd_fx = FxRate::identity(Currency::USD, common::at(9, 0));
    let input = sizing(dec!(1000000), money(dec!(5)), usd_fx);
    assert_eq!(size_position(&input, &spec), Err(SizingError::FxMismatch));
}

#[test]
fn a_zero_stage_multiplier_sizes_to_nothing() {
    let mut input = sizing(dec!(1000000), money(dec!(5)), inr_identity());
    input.stage_multiplier = Decimal::ZERO;
    assert!(matches!(
        size_position(&input, &common::equity()).unwrap(),
        SizingOutcome::TooSmall { .. }
    ));
}

#[test]
fn caps_reduce_the_quantity_and_the_planned_risk() {
    let spec = common::equity();
    let input = sizing(dec!(1000000), money(dec!(5.00)), inr_identity());
    let SizingOutcome::Sized(size) = size_position(&input, &spec).unwrap() else {
        panic!("expected a sized position");
    };
    let SizingOutcome::Sized(capped) =
        cap_quantity(&size, Quantity::new(dec!(400.7)).unwrap(), &spec).unwrap()
    else {
        panic!("expected a sized position");
    };
    assert_eq!(capped.quantity().value(), dec!(400));
    assert_eq!(capped.planned_risk(), money(dec!(2000.00)));
    assert!(matches!(
        cap_quantity(&size, Quantity::ZERO, &spec).unwrap(),
        SizingOutcome::TooSmall { .. }
    ));
}

// ---------- open risk, P&L, drawdown, R ----------

fn exposure(side: Side, mark: Decimal, protection: Protection) -> PositionExposure {
    PositionExposure {
        side,
        quantity: Quantity::new(dec!(2)).unwrap(),
        mark: Price::new(mark).unwrap(),
        protection,
        multiplier: dec!(100),
        fx: inr_identity(),
        gap_shock: Ratio::from_percent(dec!(10)).unwrap(),
    }
}

fn stop(level: Decimal) -> Protection {
    Protection::Valid {
        stop: Price::new(level).unwrap(),
    }
}

#[test]
fn open_risk_for_protected_positions() {
    // Long 2 lots, mark 6,100, stop 6,000: 2 × 100 × 100 = 20,000.
    let long = position_open_risk(&exposure(Side::Long, dec!(6100), stop(dec!(6000)))).unwrap();
    assert_eq!(long.amount, money(dec!(20000)));
    assert!(!long.unprotected);
    // Short mirrored: mark 5,950, stop 6,000.
    let short = position_open_risk(&exposure(Side::Short, dec!(5950), stop(dec!(6000)))).unwrap();
    assert_eq!(short.amount, money(dec!(10000)));
    // A trailed stop past the mark locks in profit: no open risk.
    let locked = position_open_risk(&exposure(Side::Long, dec!(6100), stop(dec!(6150)))).unwrap();
    assert_eq!(locked.amount, money(Decimal::ZERO));
}

#[test]
fn unprotected_positions_count_at_the_gap_shock() {
    let risk = position_open_risk(&exposure(Side::Long, dec!(6000), Protection::Missing)).unwrap();
    // Notional 2 × 100 × 6,000 = 1,200,000; 10% gap shock.
    assert_eq!(risk.amount, money(dec!(120000)));
    assert!(risk.unprotected);
}

#[test]
fn total_open_risk_includes_working_entries_and_refuses_mixed_currencies() {
    let entry = working_entry_risk(
        Quantity::new(dec!(100)).unwrap(),
        money(dec!(5)),
        &inr_identity(),
    )
    .unwrap();
    let total = total_open_risk([money(dec!(20000)), entry], Currency::INR).unwrap();
    assert_eq!(total, money(dec!(20500)));
    assert!(total_open_risk([Money::new(dec!(1), Currency::USD)], Currency::INR).is_err());
}

#[test]
fn pnl_drawdown_and_r_multiple() {
    assert_eq!(
        daily_pnl(money(dec!(990000)), money(dec!(1000000))).unwrap(),
        money(dec!(-10000))
    );
    let hwm = update_high_water_mark(money(dec!(1000000)), money(dec!(1100000))).unwrap();
    assert_eq!(hwm, money(dec!(1100000)));
    assert_eq!(
        drawdown(hwm, money(dec!(990000))).unwrap().value(),
        dec!(0.1)
    );
    assert_eq!(
        drawdown(money(dec!(1000000)), money(dec!(1000001))),
        Err(PortfolioError::EquityAboveHighWaterMark)
    );
    // Lost 7,500 on 1,000 shares that risked 5.00 each: −1.5R.
    assert_eq!(
        r_multiple(
            money(dec!(-7500)),
            money(dec!(5)),
            Quantity::new(dec!(1000)).unwrap()
        )
        .unwrap(),
        dec!(-1.5)
    );
    assert_eq!(
        r_multiple(money(dec!(1)), money(dec!(5)), Quantity::ZERO),
        Err(PortfolioError::ZeroPlannedRisk)
    );
}

// ---------- proposals ----------

fn draft() -> ProposalDraft {
    let at = common::at(10, 0);
    ProposalDraft {
        id: ProposalId::new_at(at),
        created_at: at,
        as_of: common::at(9, 45),
        trading_date: common::date(),
        account_mode: AccountMode::default(),
        setup_type: "pullback_in_uptrend".to_owned(),
        grade: Grade::B,
        strategy: StrategyRef {
            strategy_id: StrategyId::new_at(at),
            name: "Trend pullback".to_owned(),
            version_id: StrategyVersionId::new_at(at),
            version_number: 3,
            logic_version: "1.2.0".to_owned(),
            git_sha: "0123abc".to_owned(),
        },
        plan: plan_input(
            EntryAction::OpenLong,
            dec!(100.00),
            dec!(95.50),
            dec!(112.70),
        ),
        costs: costs(Currency::INR, dec!(100), dec!(20)),
        slippage: SlippageAssumption::new("slip-v1", dec!(0.30)).unwrap(),
        probabilities: OutcomeProbabilities::new(
            dec!(0.40),
            dec!(0.45),
            dec!(0.15),
            "empirical-v1",
            42,
        )
        .unwrap(),
        time_exit_pnl_per_unit: dec!(1.00),
        explanation: Explanation {
            reasons: vec![Reason {
                factor: "trend_strength_z".to_owned(),
                value: FactorValue::Number(dec!(1.8)),
                direction: ReasonDirection::Supports,
            }],
            strongest_argument_against: "Sector momentum is fading".to_owned(),
            ai_review: None,
        },
        reproducibility: Reproducibility {
            snapshot_id: SnapshotId::new_at(at),
            feature_set_version: "features-v1".to_owned(),
            calendar_version: "nse-2026.1".to_owned(),
        },
    }
}

#[test]
fn a_complete_proposal_computes_its_own_numbers() {
    let proposal = TradeProposal::build(draft(), &common::equity()).unwrap();
    assert_eq!(proposal.account_mode(), AccountMode::Paper);
    assert_eq!(proposal.economics().rr_net(), dec!(2.5));
    assert_eq!(proposal.expected_value().in_r(), dec!(0.58));
    assert_eq!(proposal.instrument().symbol, "TEST-EQ");
    let json = serde_json::to_value(&proposal).unwrap();
    assert_eq!(json["economics"]["risk_net"], "5.00");
    assert_eq!(json["plan"]["action"], "open_long");
}

#[test]
fn incomplete_proposals_are_refused() {
    let spec = common::equity();

    let mut d = draft();
    d.explanation.strongest_argument_against = "  ".to_owned();
    assert_eq!(
        TradeProposal::build(d, &spec),
        Err(ProposalError::Incomplete(
            "explanation.strongest_argument_against"
        ))
    );

    let mut d = draft();
    d.explanation.reasons.clear();
    assert_eq!(
        TradeProposal::build(d, &spec),
        Err(ProposalError::Incomplete("explanation.reasons"))
    );

    let mut d = draft();
    d.strategy.git_sha.clear();
    assert_eq!(
        TradeProposal::build(d, &spec),
        Err(ProposalError::Incomplete("strategy.git_sha"))
    );

    let mut d = draft();
    d.as_of = common::at(10, 1);
    assert_eq!(
        TradeProposal::build(d, &spec),
        Err(ProposalError::AsOfAfterCreation)
    );

    let mut d = draft();
    d.trading_date = chrono::NaiveDate::from_ymd_opt(2025, 12, 31).unwrap();
    assert_eq!(
        TradeProposal::build(d, &spec),
        Err(ProposalError::InstrumentNotEffective)
    );

    let mut d = draft();
    d.plan.stop = dec!(101);
    assert_eq!(
        TradeProposal::build(d, &spec),
        Err(ProposalError::Plan(PlanDefect::StopOnWrongSide))
    );
}
