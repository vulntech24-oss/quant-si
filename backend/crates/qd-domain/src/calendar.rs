//! Exchange trading calendars and data-quality checks for daily bars
//! (ADR 0015).
//!
//! A calendar is data: weekends plus a list of holidays for the years it
//! covers, with its source. Outside those years it answers "unknown", and
//! callers fall back to their calendar-day rules rather than guessing.

use std::collections::BTreeSet;

use chrono::{Datelike, NaiveDate, Weekday};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::instrument::CalendarId;
use crate::market::Bar;

/// One exchange calendar as stored in the data file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CalendarData {
    /// Calendar ids this list applies to (for example `nse`, `bse`, `nfo`).
    pub ids: Vec<String>,
    /// Version of this list.
    pub version: u32,
    /// First date covered.
    pub covers_from: NaiveDate,
    /// Last date covered.
    pub covers_through: NaiveDate,
    /// Where the list comes from.
    pub source: String,
    /// When it was checked.
    pub checked_on: NaiveDate,
    /// Weekday exchange holidays (weekend holidays need not be listed).
    pub holidays: Vec<NaiveDate>,
    /// Weekend dates with a special session (for example Muhurat trading).
    #[serde(default)]
    pub special_sessions: Vec<NaiveDate>,
}

/// The calendar file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CalendarSet {
    /// Calendars.
    pub calendars: Vec<CalendarData>,
}

/// Why a calendar is invalid.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CalendarError {
    /// Empty or inverted coverage, a holiday outside it, or no ids.
    #[error("calendar {0}: {1}")]
    Invalid(String, String),
    /// Two calendars claim one id.
    #[error("calendar id {0} is defined twice")]
    Duplicate(String),
}

/// A validated trading calendar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TradingCalendar {
    data: CalendarData,
    holidays: BTreeSet<NaiveDate>,
    special: BTreeSet<NaiveDate>,
}

impl TradingCalendar {
    /// Validates one calendar.
    pub fn new(data: CalendarData) -> Result<Self, CalendarError> {
        let name = data.ids.join("/");
        let bad = |why: &str| CalendarError::Invalid(name.clone(), why.to_owned());
        if data.ids.is_empty() {
            return Err(bad("no ids"));
        }
        if data.covers_from > data.covers_through {
            return Err(bad("coverage ends before it starts"));
        }
        let within = |d: &NaiveDate| (data.covers_from..=data.covers_through).contains(d);
        if !data
            .holidays
            .iter()
            .chain(&data.special_sessions)
            .all(within)
        {
            return Err(bad("a date lies outside the coverage"));
        }
        Ok(Self {
            holidays: data.holidays.iter().copied().collect(),
            special: data.special_sessions.iter().copied().collect(),
            data,
        })
    }

    /// The raw data.
    #[must_use]
    pub const fn data(&self) -> &CalendarData {
        &self.data
    }

    /// Whether `date` is a trading day; `None` outside the coverage.
    #[must_use]
    pub fn is_trading_day(&self, date: NaiveDate) -> Option<bool> {
        if date < self.data.covers_from || date > self.data.covers_through {
            return None;
        }
        if self.special.contains(&date) {
            return Some(true);
        }
        let weekend = matches!(date.weekday(), Weekday::Sat | Weekday::Sun);
        Some(!weekend && !self.holidays.contains(&date))
    }

    /// Trading days strictly between `after` and `before`; `None` if any
    /// day in between is outside the coverage.
    #[must_use]
    pub fn trading_days_between(
        &self,
        after: NaiveDate,
        before: NaiveDate,
    ) -> Option<Vec<NaiveDate>> {
        let mut days = Vec::new();
        let mut d = after.succ_opt()?;
        while d < before {
            if self.is_trading_day(d)? {
                days.push(d);
            }
            d = d.succ_opt()?;
        }
        Some(days)
    }
}

/// Every calendar, by id.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Calendars(Vec<TradingCalendar>);

impl Calendars {
    /// Validates a calendar file.
    pub fn new(set: CalendarSet) -> Result<Self, CalendarError> {
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for data in set.calendars {
            for id in &data.ids {
                if !seen.insert(id.clone()) {
                    return Err(CalendarError::Duplicate(id.clone()));
                }
            }
            out.push(TradingCalendar::new(data)?);
        }
        Ok(Self(out))
    }

    /// The calendar for an id.
    #[must_use]
    pub fn get(&self, id: &CalendarId) -> Option<&TradingCalendar> {
        self.0.iter().find(|c| c.data.ids.contains(&id.0))
    }

    /// All calendars.
    #[must_use]
    pub fn all(&self) -> &[TradingCalendar] {
        &self.0
    }
}

/// Limits for [`check_bars`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QualityLimits {
    /// A close more than this fraction away from the previous close is
    /// suspect (a split, a bonus, or a bad print). Indian circuit limits
    /// stop most stocks at 20% a day.
    pub max_close_jump: Decimal,
}

impl Default for QualityLimits {
    fn default() -> Self {
        Self {
            max_close_jump: Decimal::new(20, 2),
        }
    }
}

/// A problem found in a series of daily bars.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "issue", rename_all = "snake_case")]
pub enum DataIssue {
    /// A trading day with no bar.
    MissingDay {
        /// The day.
        date: NaiveDate,
    },
    /// A bar on a day the exchange was closed.
    BarOnHoliday {
        /// The day.
        date: NaiveDate,
    },
    /// The close moved more than the limit from the previous close.
    PriceJump {
        /// The day.
        date: NaiveDate,
        /// Previous close.
        previous: Decimal,
        /// This close.
        close: Decimal,
    },
    /// The calendar does not cover these bars, so gaps were not checked.
    CalendarUnknown {
        /// First uncovered date seen.
        date: NaiveDate,
    },
}

impl DataIssue {
    /// The issue's date.
    #[must_use]
    pub const fn date(&self) -> NaiveDate {
        match self {
            Self::MissingDay { date }
            | Self::BarOnHoliday { date }
            | Self::PriceJump { date, .. }
            | Self::CalendarUnknown { date } => *date,
        }
    }
}

/// Checks ordered bars, optionally continuing from the last stored bar:
/// gaps and holiday bars against the calendar, and close-to-close jumps.
#[must_use]
pub fn check_bars(
    previous: Option<&Bar>,
    bars: &[Bar],
    calendar: Option<&TradingCalendar>,
    limits: QualityLimits,
) -> Vec<DataIssue> {
    let mut issues = Vec::new();
    let mut unknown_reported = false;
    let mut last = previous.copied();
    for bar in bars {
        let date = bar.date();
        match calendar.map(|c| (c, c.is_trading_day(date))) {
            Some((_, Some(false))) => issues.push(DataIssue::BarOnHoliday { date }),
            Some((_, None)) | None if !unknown_reported => {
                unknown_reported = true;
                issues.push(DataIssue::CalendarUnknown { date });
            }
            _ => {}
        }
        if let Some(prev) = last {
            if let Some(Some(missing)) = calendar.map(|c| c.trading_days_between(prev.date(), date))
            {
                issues.extend(
                    missing
                        .into_iter()
                        .map(|date| DataIssue::MissingDay { date }),
                );
            }
            let p = prev.close().value();
            let c = bar.close().value();
            if p > Decimal::ZERO && ((c - p).abs() / p) > limits.max_close_jump {
                issues.push(DataIssue::PriceJump {
                    date,
                    previous: p,
                    close: c,
                });
            }
        }
        last = Some(*bar);
    }
    issues
}
