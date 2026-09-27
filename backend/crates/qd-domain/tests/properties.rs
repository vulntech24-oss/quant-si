//! Property tests for rounding, economics, expected value, sizing and risk rules.

// Test code: a failed unwrap is a failed test. The workspace-wide unwrap/expect
// ban targets runtime code, and clippy's allow-unwrap-in-tests does not reach
// helper functions in integration-test files.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use proptest::prelude::*;
use qd_domain::action::{EntryAction, Side};
use qd_domain::economics::{
    CostEstimate, CostLine, ExpectedValue, OutcomeProbabilities, SlippageAssumption, UnitEconomics,
};
use qd_domain::instrument::{InstrumentSpec, Rounding};
use qd_domain::num::{Currency, FxRate, Money, Price, Quantity, Ratio};
use qd_domain::order_rules::check_exit_quantity;
use qd_domain::plan::{EntryOrderType, PlanDefect, TradePlan, TradePlanInput};
use qd_domain::portfolio::{PositionExposure, Protection, position_open_risk};
use qd_domain::sizing::{SizingInput, SizingOutcome, size_position};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

fn tick() -> impl Strategy<Value = Decimal> {
    prop_oneof![
        Just(dec!(0.01)),
        Just(dec!(0.05)),
        Just(dec!(0.25)),
        Just(dec!(1)),
        Just(dec!(10)),
    ]
}

fn spec_with_tick(tick: Decimal) -> InstrumentSpec {
    let mut data = common::equity_data();
    data.tick_size = tick;
    InstrumentSpec::new(data).unwrap()
}

fn plan(action: EntryAction, entry: Decimal, stop: Decimal, target: Decimal) -> TradePlanInput {
    TradePlanInput {
        action,
        entry_type: EntryOrderType::Limit,
        entry,
        stop,
        target,
        max_holding_days: 10,
        invalidation: vec![],
    }
}

fn costs(per_lot: Decimal) -> CostEstimate {
    CostEstimate::new(
        "cost-v1",
        Currency::INR,
        Quantity::new(Decimal::ONE).unwrap(),
        vec![CostLine {
            name: "round trip".to_owned(),
            amount: per_lot,
        }],
    )
    .unwrap()
}

proptest! {
    #[test]
    fn price_rounding_is_tick_aligned_and_directional(tick in tick(), raw in 1_i64..=10_000_000_000) {
        let spec = spec_with_tick(tick);
        let value = Decimal::new(raw, 4);

        let up = spec.round_price(value, Rounding::Up).unwrap();
        prop_assert!(spec.is_tick_aligned(up));
        prop_assert!(up.value() >= value);
        prop_assert!(up.value() - value < tick);

        match spec.round_price(value, Rounding::Down) {
            Ok(down) => {
                prop_assert!(spec.is_tick_aligned(down));
                prop_assert!(down.value() <= value);
                prop_assert!(value - down.value() < tick);
            }
            // Only a value below one tick rounds down to zero, which is not a price.
            Err(_) => prop_assert!(value < tick),
        }
    }

    #[test]
    fn long_plans_round_conservatively(
        tick in tick(),
        entry in 100_000_i64..=100_000_000,
        risk in 1_i64..=5_000_000,
        reward in 1_i64..=10_000_000,
    ) {
        prop_assume!(entry > risk);
        let spec = spec_with_tick(tick);
        let (e, s, t) = (
            Decimal::new(entry, 4),
            Decimal::new(entry - risk, 4),
            Decimal::new(entry + reward, 4),
        );
        match TradePlan::new(plan(EntryAction::OpenLong, e, s, t), &spec) {
            Ok(p) => {
                let (pe, ps, pt) = (p.entry().price.value(), p.stop().value(), p.target().value());
                prop_assert!(ps < pe && pe < pt);
                prop_assert!(pe >= e && ps <= s && pt <= t);
                for level in [p.entry().price, p.stop(), p.target()] {
                    prop_assert!(spec.is_tick_aligned(level));
                }
                prop_assert_eq!(p.risk_points(), pe - ps);
                prop_assert_eq!(p.reward_points(), pt - pe);
            }
            // Rounding may push the stop to zero or collapse the target onto the entry.
            Err(defect) => {
                let expected = matches!(
                    defect,
                    PlanDefect::TargetOnWrongSide | PlanDefect::NonPositiveLevel { .. }
                );
                prop_assert!(expected, "unexpected defect {defect:?}");
            }
        }
    }

    #[test]
    fn short_economics_mirror_long_economics(
        entry in 1_000_i64..=100_000,
        risk in 1_i64..=999,
        reward in 1_i64..=5_000,
        cost in 0_i64..=2_000,
        slip in 0_i64..=50,
    ) {
        prop_assume!(entry > reward);
        let spec = common::crude_future();
        let e = Decimal::from(entry);
        let (r, w) = (Decimal::from(risk), Decimal::from(reward));
        let slippage = SlippageAssumption::new("slip-v1", Decimal::from(slip)).unwrap();
        let costs = costs(Decimal::from(cost));

        let long = TradePlan::new(plan(EntryAction::OpenLong, e, e - r, e + w), &spec).unwrap();
        let short = TradePlan::new(plan(EntryAction::OpenShort, e, e + r, e - w), &spec).unwrap();
        let long = UnitEconomics::compute(&long, &spec, &costs, &slippage);
        let short = UnitEconomics::compute(&short, &spec, &costs, &slippage);
        match (long, short) {
            (Ok(l), Ok(s)) => {
                prop_assert_eq!(l.risk_net(), s.risk_net());
                prop_assert_eq!(l.reward_net(), s.reward_net());
                prop_assert_eq!(l.rr_net(), s.rr_net());
            }
            (Err(l), Err(s)) => prop_assert_eq!(l, s),
            (l, s) => {
                return Err(TestCaseError::fail(format!("long {l:?} and short {s:?} disagree")));
            }
        }
    }

    #[test]
    fn net_economics_are_never_better_than_gross(
        entry in 1_000_i64..=100_000,
        risk in 1_i64..=999,
        reward in 1_i64..=5_000,
        cost in 0_i64..=2_000,
        slip in 0_i64..=50,
    ) {
        let spec = common::crude_future();
        let e = Decimal::from(entry);
        let p = TradePlan::new(
            plan(EntryAction::OpenLong, e, e - Decimal::from(risk), e + Decimal::from(reward)),
            &spec,
        )
        .unwrap();
        let slippage = SlippageAssumption::new("slip-v1", Decimal::from(slip)).unwrap();
        if let Ok(econ) = UnitEconomics::compute(&p, &spec, &costs(Decimal::from(cost)), &slippage) {
            prop_assert!(econ.risk_net() >= econ.risk_gross());
            prop_assert!(econ.reward_net() <= econ.reward_gross());
            prop_assert!(econ.reward_net() > Decimal::ZERO);
            let gross_rr = econ.reward_gross() / econ.risk_gross();
            prop_assert!(econ.rr_net() <= gross_rr);
        }
    }

    #[test]
    fn expected_value_is_consistent(
        a in 0_u32..=100,
        b in 0_u32..=100,
        time_pnl in -1_000_i64..=1_000,
    ) {
        prop_assume!(a + b <= 100);
        let spec = common::equity();
        let p = TradePlan::new(plan(EntryAction::OpenLong, dec!(100), dec!(95.50), dec!(112.70)), &spec)
            .unwrap();
        let econ = UnitEconomics::compute(
            &p,
            &spec,
            &costs(dec!(0.20)),
            &SlippageAssumption::new("slip-v1", dec!(0.30)).unwrap(),
        )
        .unwrap();
        let hundred = Decimal::ONE_HUNDRED;
        let probs = OutcomeProbabilities::new(
            Decimal::from(a) / hundred,
            Decimal::from(b) / hundred,
            Decimal::from(100 - a - b) / hundred,
            "m",
            30,
        )
        .unwrap();
        let time_pnl = Decimal::new(time_pnl, 2);
        let ev = ExpectedValue::compute(&econ, &probs, time_pnl).unwrap();

        let expected = probs.p_target() * econ.reward_net() - probs.p_stop() * econ.risk_net()
            + probs.p_time() * time_pnl;
        prop_assert_eq!(ev.per_unit(), expected);
        let back = ev.in_r() * econ.risk_net();
        prop_assert!((back - ev.per_unit()).abs() <= dec!(0.000000000000000001));
        if a == 100 {
            prop_assert_eq!(ev.per_unit(), econ.reward_net());
        }
    }

    #[test]
    fn sizing_never_exceeds_the_risk_budget_and_is_maximal(
        equity in 10_000_i64..=100_000_000,
        rpu in 1_i64..=10_000_000,
        stage in prop_oneof![Just(Decimal::ZERO), Just(dec!(0.25)), Just(Decimal::ONE)],
        crypto in any::<bool>(),
    ) {
        let spec = if crypto { common::crypto_spot() } else { common::equity() };
        let at = common::at(9, 0);
        let input = SizingInput {
            equity: Money::new(Decimal::from(equity), Currency::INR),
            risk_per_trade: Ratio::from_percent(dec!(0.5)).unwrap(),
            stage_multiplier: stage,
            risk_net_per_unit: Money::new(Decimal::new(rpu, 2), spec.currency),
            fx: if crypto {
                FxRate::new(Currency::USDT, Currency::INR, dec!(85.10), at).unwrap()
            } else {
                FxRate::identity(Currency::INR, at)
            },
        };
        let step = spec.quantity_step;
        match size_position(&input, &spec).unwrap() {
            SizingOutcome::Sized(size) => {
                let budget = size.risk_budget().amount;
                let per_unit = size.risk_per_unit().amount;
                prop_assert!(size.planned_risk().amount <= budget);
                prop_assert!(spec.is_valid_order_quantity(size.quantity()));
                prop_assert!((size.quantity().value() + step) * per_unit > budget);
            }
            SizingOutcome::TooSmall { quantity, min } => {
                prop_assert!(quantity < min);
            }
        }
    }

    #[test]
    fn protected_open_risk_is_never_negative(
        mark in 1_i64..=10_000_000,
        stop in 1_i64..=10_000_000,
        qty in 0_i64..=1_000,
        short in any::<bool>(),
    ) {
        let exposure = PositionExposure {
            side: if short { Side::Short } else { Side::Long },
            quantity: Quantity::new(Decimal::from(qty)).unwrap(),
            mark: Price::new(Decimal::new(mark, 2)).unwrap(),
            protection: Protection::Valid { stop: Price::new(Decimal::new(stop, 2)).unwrap() },
            multiplier: dec!(100),
            fx: FxRate::identity(Currency::INR, common::at(9, 0)),
            gap_shock: Ratio::from_percent(dec!(10)).unwrap(),
        };
        let risk = position_open_risk(&exposure).unwrap();
        prop_assert!(risk.amount.amount >= Decimal::ZERO);
        prop_assert!(!risk.unprotected);
    }

    #[test]
    fn accepted_exits_never_exceed_the_open_quantity(open in 0_i64..=1_000_000, exit in 0_i64..=1_000_000) {
        let open = Quantity::new(Decimal::new(open, 3)).unwrap();
        let exit = Quantity::new(Decimal::new(exit, 3)).unwrap();
        if check_exit_quantity(open, exit).is_ok() {
            prop_assert!(exit <= open);
            prop_assert!(!exit.is_zero());
        }
    }
}
