//! Bars and point-in-time bar series.

// Test code: a failed unwrap is a failed test. The workspace-wide unwrap/expect
// ban targets runtime code, and clippy's allow-unwrap-in-tests does not reach
// helper functions in integration-test files.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use chrono::{Days, NaiveDate};
use qd_domain::action::{TradeAction, Transfer};
use qd_domain::ids::InstrumentId;
use qd_domain::market::{Bar, BarData, BarError, BarSeries};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

fn day(n: u64) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 3, 2).unwrap() + Days::new(n)
}

fn bar(n: u64, close: Decimal) -> Bar {
    Bar::new(BarData {
        date: day(n),
        open: close,
        high: close + dec!(1),
        low: close - dec!(1),
        close,
        volume: dec!(1000),
    })
    .unwrap()
}

#[test]
fn bars_reject_inconsistent_values() {
    let mut data = BarData {
        date: day(0),
        open: dec!(100),
        high: dec!(99),
        low: dec!(98),
        close: dec!(99),
        volume: dec!(1),
    };
    assert_eq!(
        Bar::new(data),
        Err(BarError::InconsistentOhlc { date: day(0) })
    );
    data.high = dec!(101);
    data.volume = dec!(-1);
    assert_eq!(
        Bar::new(data),
        Err(BarError::NegativeVolume { date: day(0) })
    );
    data.volume = dec!(1);
    data.low = dec!(0);
    assert!(Bar::new(data).is_err());
}

#[test]
fn invariant_09_series_refuse_bars_after_the_last_completed_date() {
    let id = InstrumentId::new_at(common::at(0, 0));
    let bars = vec![bar(0, dec!(100)), bar(1, dec!(101)), bar(2, dec!(102))];
    assert_eq!(
        BarSeries::new(id, day(1), bars.clone()),
        Err(BarError::NotCompleted {
            date: day(2),
            last_completed: day(1)
        })
    );
    let series = BarSeries::new(id, day(2), bars).unwrap();
    let earlier = series.as_of(day(1));
    assert_eq!(earlier.bars().len(), 2);
    assert_eq!(earlier.last_completed(), day(1));
    assert_eq!(earlier.last().unwrap().date(), day(1));
}

#[test]
fn series_must_be_strictly_increasing() {
    let id = InstrumentId::new_at(common::at(0, 0));
    let bars = vec![bar(1, dec!(100)), bar(1, dec!(101))];
    assert_eq!(
        BarSeries::new(id, day(5), bars),
        Err(BarError::NotIncreasing { date: day(1) })
    );
}

#[test]
fn bars_round_trip_through_json_and_revalidate() {
    let b = bar(0, dec!(100));
    let json = serde_json::to_string(&b).unwrap();
    assert_eq!(serde_json::from_str::<Bar>(&json).unwrap(), b);
    let broken = json.replace(r#""high":"101""#, r#""high":"50""#);
    assert!(serde_json::from_str::<Bar>(&broken).is_err());
}

#[test]
fn transfers_follow_the_action_not_the_side() {
    assert_eq!(TradeAction::OpenLong.transfer(), Transfer::Purchase);
    assert_eq!(TradeAction::CloseShort.transfer(), Transfer::Purchase);
    assert_eq!(TradeAction::CloseLong.transfer(), Transfer::Disposal);
    assert_eq!(TradeAction::OpenShort.transfer(), Transfer::Disposal);
}
