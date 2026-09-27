//! Review and calibration: exact decimal results on hand-built journals.

// Test code: a failed unwrap is a failed test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use qd_app::review::{Prediction, ReviewCriteria, paper_checks, prediction, review};
use qd_app::session::{AccountBook, DayRecord, TradeRecord};
use qd_domain::action::Side;
use qd_domain::ids::{AccountId, DecisionId, PositionId, StrategyVersionId};
use qd_domain::num::{Currency, Money, Quantity};
use qd_domain::outcome::ExitReason;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

fn at() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 5, 1, 10, 0, 0).unwrap()
}

fn date(d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 5, d).unwrap()
}

fn trade(decision: DecisionId, reason: ExitReason, r: Decimal) -> TradeRecord {
    TradeRecord {
        position: PositionId::new_at(at()),
        decision: Some(decision),
        setup_type: "s".to_owned(),
        instrument: "X".to_owned(),
        side: Side::Long,
        opened_on: date(1),
        closed_on: date(2),
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

fn day(account: AccountId, d: u32, equity: Decimal, trades: Vec<TradeRecord>) -> DayRecord {
    DayRecord {
        account,
        date: date(d),
        equity,
        book: AccountBook::new(account, Money::new(dec!(100), Currency::INR), date(1)),
        trades,
        decisions: BTreeMap::new(),
        unprotected: 0,
        mismatches: 0,
    }
}

#[test]
fn predictions_meet_their_outcomes_with_exact_scores() {
    let account = AccountId::new_at(at());
    let version = StrategyVersionId::new_at(at());
    let p = |p_target: Decimal, ev_r: Decimal| Prediction {
        decision: DecisionId::new_at(at()),
        version,
        p: [p_target, dec!(0.3), Decimal::ONE - p_target - dec!(0.3)],
        ev_r,
    };
    let a = p(dec!(0.5), dec!(0.4));
    let b = p(dec!(0.5), dec!(0.4));
    let unmatched = p(dec!(0.1), dec!(0.2)); // decided, never closed
    let days = vec![
        day(account, 1, dec!(100), vec![]),
        day(
            account,
            2,
            dec!(110),
            vec![trade(a.decision, ExitReason::TargetHit, dec!(2))],
        ),
        day(
            account,
            3,
            dec!(99),
            vec![trade(b.decision, ExitReason::StopHit, dec!(-1))],
        ),
    ];
    let report = review(account, &[a, b, unmatched], &days);
    let v = &report.versions[0];
    assert_eq!((v.entries, v.trades), (3, 2));
    assert_eq!(v.realized, [dec!(0.5), dec!(0.5), Decimal::ZERO]);
    assert_eq!(v.predicted[0], dec!(0.5));
    // Brier: ((0.5 − 1)² + (0.5 − 0)²) / 2 = 0.25.
    assert_eq!(v.brier, dec!(0.25));
    assert_eq!((v.predicted_ev_r, v.realized_r), (dec!(0.4), dec!(0.5)));
    // Both trades fall in the [0.4, 0.6) bin.
    let bin = v.bins.iter().find(|b| b.count > 0).unwrap();
    assert_eq!(
        (bin.from, bin.count, bin.realized),
        (dec!(0.4), 2, dec!(0.5))
    );
    // Drawdown: from 110 to 99 is 10%.
    assert_eq!(report.max_drawdown, dec!(0.1));
    assert_eq!(report.operational_incidents, 0);

    let criteria = ReviewCriteria {
        min_trades: 2,
        min_days: 3,
        min_expectancy_r: dec!(0.05),
        max_expectancy_shortfall_r: dec!(0.3),
        max_drawdown: dec!(0.10),
        max_brier: dec!(0.25),
    };
    let checks = paper_checks(&report, v, &criteria);
    assert!(checks.iter().all(|c| c.passed), "{checks:?}");
    let stricter = ReviewCriteria {
        max_drawdown: dec!(0.05),
        ..criteria
    };
    let failed: Vec<&str> = paper_checks(&report, v, &stricter)
        .into_iter()
        .filter(|c| !c.passed)
        .map(|c| c.name)
        .collect();
    assert_eq!(failed, vec!["drawdown"]);
}

#[test]
fn operational_incidents_fail_a_paper_review() {
    let account = AccountId::new_at(at());
    let mut bad = day(account, 1, dec!(100), vec![]);
    bad.mismatches = 1;
    let report = review(account, &[], &[bad]);
    assert_eq!(report.operational_incidents, 1);
}

#[test]
fn only_entry_decisions_of_the_account_are_predictions() {
    let account = AccountId::new_at(at());
    let entry = serde_json::json!({
        "id": DecisionId::new_at(at()),
        "account": account,
        "strategy": { "version_id": StrategyVersionId::new_at(at()) },
        "outcome": { "outcome": "enter", "action": "open_long" },
        "proposal": {
            "probabilities": { "p_target": "0.4", "p_stop": "0.4", "p_time": "0.2" },
            "expected_value": { "in_r": "0.25", "per_unit": "1" }
        }
    });
    let p = prediction(account, &entry).unwrap();
    assert_eq!(p.p, [dec!(0.4), dec!(0.4), dec!(0.2)]);
    assert_eq!(p.ev_r, dec!(0.25));
    assert!(prediction(AccountId::new_at(at()), &entry).is_none());
    let mut no_trade = entry;
    no_trade["outcome"] = serde_json::json!({ "outcome": "no_trade", "reason": {} });
    assert!(prediction(account, &no_trade).is_none());
}
