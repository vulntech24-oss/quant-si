//! Trading calendars and data-quality checks (ADR 0015).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use chrono::NaiveDate;
use qd_domain::calendar::{CalendarSet, Calendars, DataIssue, QualityLimits, check_bars};
use qd_domain::instrument::CalendarId;
use qd_domain::market::{Bar, BarData};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

const FILE: &str = include_str!("../../../config/calendars/india.toml");

fn date(s: &str) -> NaiveDate {
    s.parse().unwrap()
}

fn calendars() -> Calendars {
    Calendars::new(toml::from_str::<CalendarSet>(FILE).unwrap()).unwrap()
}

fn bar(d: &str, close: Decimal) -> Bar {
    Bar::new(BarData {
        date: date(d),
        open: close,
        high: close,
        low: close,
        close,
        volume: dec!(1),
    })
    .unwrap()
}

#[test]
fn the_shipped_calendars_know_weekends_holidays_and_their_coverage() {
    let all = calendars();
    let nse = all.get(&CalendarId("nse".to_owned())).unwrap();
    assert_eq!(all.get(&CalendarId("bse".to_owned())), Some(nse));
    assert_eq!(nse.is_trading_day(date("2026-10-02")), Some(false)); // Gandhi Jayanti
    assert_eq!(nse.is_trading_day(date("2026-10-03")), Some(false)); // Saturday
    assert_eq!(nse.is_trading_day(date("2026-10-05")), Some(true));
    assert_eq!(nse.is_trading_day(date("2026-11-08")), Some(true)); // Muhurat Sunday
    assert_eq!(nse.is_trading_day(date("2027-01-04")), None);
    let mcx = all.get(&CalendarId("mcx".to_owned())).unwrap();
    // MCX trades on Holi (evening session); NSE does not.
    assert_eq!(mcx.is_trading_day(date("2026-03-03")), Some(true));
    assert_eq!(nse.is_trading_day(date("2026-03-03")), Some(false));
    assert!(
        all.all()
            .iter()
            .all(|c| c.data().source.contains("zerodha.com"))
    );
}

#[test]
fn gaps_holiday_bars_and_jumps_are_reported() {
    let all = calendars();
    let nse = all.get(&CalendarId("nse".to_owned()));
    // Thu 1 Oct, (Fri 2 Oct holiday), Mon 5 Oct, [Tue 6 missing], Wed 7 Oct.
    let bars = [
        bar("2026-10-01", dec!(100)),
        bar("2026-10-05", dec!(101)),
        bar("2026-10-07", dec!(130)),
    ];
    let issues = check_bars(None, &bars, nse, QualityLimits::default());
    assert_eq!(
        issues,
        vec![
            DataIssue::MissingDay {
                date: date("2026-10-06")
            },
            DataIssue::PriceJump {
                date: date("2026-10-07"),
                previous: dec!(101),
                close: dec!(130)
            },
        ]
    );
    let on_holiday = [bar("2026-10-02", dec!(100))];
    assert_eq!(
        check_bars(Some(&bars[0]), &on_holiday, nse, QualityLimits::default()),
        vec![DataIssue::BarOnHoliday {
            date: date("2026-10-02")
        }]
    );
    // Outside the coverage nothing is guessed.
    let later = [bar("2027-01-04", dec!(100))];
    assert_eq!(
        check_bars(None, &later, nse, QualityLimits::default()),
        vec![DataIssue::CalendarUnknown {
            date: date("2027-01-04")
        }]
    );
}

#[test]
fn a_calendar_with_dates_outside_its_coverage_is_refused() {
    let bad = FILE.replacen("\"2026-01-15\"", "\"2027-01-15\"", 1);
    assert!(Calendars::new(toml::from_str::<CalendarSet>(&bad).unwrap()).is_err());
    let twice = format!(
        "{FILE}\n[[calendars]]\nids = [\"nse\"]\nversion = 1\ncovers_from = \"2026-01-01\"\ncovers_through = \"2026-12-31\"\nsource = \"x\"\nchecked_on = \"2026-01-01\"\nholidays = []\n"
    );
    assert!(Calendars::new(toml::from_str::<CalendarSet>(&twice).unwrap()).is_err());
}
