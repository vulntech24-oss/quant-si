//! Daily bars and point-in-time bar series (spec §2 "completed daily bars", INV-09).

use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ids::InstrumentId;
use crate::num::{NumError, Price};

/// Why a bar or a bar series is invalid.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BarError {
    /// High is below open, close or low, or low is above open or close.
    #[error("bar on {date} has inconsistent OHLC values")]
    InconsistentOhlc {
        /// Bar date.
        date: NaiveDate,
    },
    /// Volume is negative.
    #[error("bar on {date} has negative volume")]
    NegativeVolume {
        /// Bar date.
        date: NaiveDate,
    },
    /// Bars are not in strictly increasing date order.
    #[error("bars are not strictly increasing by date at {date}")]
    NotIncreasing {
        /// First out-of-order date.
        date: NaiveDate,
    },
    /// A bar is dated after the last completed trading date (look-ahead).
    #[error("bar on {date} is after the last completed trading date {last_completed}")]
    NotCompleted {
        /// Bar date.
        date: NaiveDate,
        /// Last completed trading date.
        last_completed: NaiveDate,
    },
    /// A price is not positive.
    #[error(transparent)]
    Num(#[from] NumError),
}

/// Raw bar values, as a data source delivers them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BarData {
    /// Trading date on the venue calendar.
    pub date: NaiveDate,
    /// Open.
    pub open: Decimal,
    /// High.
    pub high: Decimal,
    /// Low.
    pub low: Decimal,
    /// Close.
    pub close: Decimal,
    /// Volume in units.
    pub volume: Decimal,
}

/// One validated daily bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "BarData", into = "BarData")]
pub struct Bar {
    date: NaiveDate,
    open: Price,
    high: Price,
    low: Price,
    close: Price,
    volume: Decimal,
}

impl TryFrom<BarData> for Bar {
    type Error = BarError;

    fn try_from(data: BarData) -> Result<Self, Self::Error> {
        Self::new(data)
    }
}

impl From<Bar> for BarData {
    fn from(bar: Bar) -> Self {
        Self {
            date: bar.date,
            open: bar.open.value(),
            high: bar.high.value(),
            low: bar.low.value(),
            close: bar.close.value(),
            volume: bar.volume,
        }
    }
}

impl Bar {
    /// Validates prices (positive, low ≤ open/close ≤ high) and volume (≥ 0).
    pub fn new(data: BarData) -> Result<Self, BarError> {
        let (open, high, low, close) = (
            Price::new(data.open)?,
            Price::new(data.high)?,
            Price::new(data.low)?,
            Price::new(data.close)?,
        );
        if low > open || low > close || high < open || high < close {
            return Err(BarError::InconsistentOhlc { date: data.date });
        }
        if data.volume < Decimal::ZERO {
            return Err(BarError::NegativeVolume { date: data.date });
        }
        Ok(Self {
            date: data.date,
            open,
            high,
            low,
            close,
            volume: data.volume,
        })
    }

    /// Trading date.
    #[must_use]
    pub const fn date(&self) -> NaiveDate {
        self.date
    }

    /// Open.
    #[must_use]
    pub const fn open(&self) -> Price {
        self.open
    }

    /// High.
    #[must_use]
    pub const fn high(&self) -> Price {
        self.high
    }

    /// Low.
    #[must_use]
    pub const fn low(&self) -> Price {
        self.low
    }

    /// Close.
    #[must_use]
    pub const fn close(&self) -> Price {
        self.close
    }

    /// Volume.
    #[must_use]
    pub const fn volume(&self) -> Decimal {
        self.volume
    }
}

/// Completed daily bars for one instrument, oldest first, none after
/// `last_completed` (INV-09). Strategies only ever receive this type.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BarSeries {
    instrument: InstrumentId,
    last_completed: NaiveDate,
    bars: Vec<Bar>,
}

impl BarSeries {
    /// Builds a series. Rejects out-of-order bars and any bar after `last_completed`.
    pub fn new(
        instrument: InstrumentId,
        last_completed: NaiveDate,
        bars: Vec<Bar>,
    ) -> Result<Self, BarError> {
        for pair in bars.windows(2) {
            if let [a, b] = pair {
                if b.date <= a.date {
                    return Err(BarError::NotIncreasing { date: b.date });
                }
            }
        }
        if let Some(last) = bars.last() {
            if last.date > last_completed {
                return Err(BarError::NotCompleted {
                    date: last.date,
                    last_completed,
                });
            }
        }
        Ok(Self {
            instrument,
            last_completed,
            bars,
        })
    }

    /// Instrument.
    #[must_use]
    pub const fn instrument(&self) -> InstrumentId {
        self.instrument
    }

    /// The last completed trading date the series was built for.
    #[must_use]
    pub const fn last_completed(&self) -> NaiveDate {
        self.last_completed
    }

    /// The bars, oldest first.
    #[must_use]
    pub fn bars(&self) -> &[Bar] {
        &self.bars
    }

    /// The most recent bar.
    #[must_use]
    pub fn last(&self) -> Option<&Bar> {
        self.bars.last()
    }

    /// The series as it was known at the end of `date`: bars up to and including it.
    #[must_use]
    pub fn as_of(&self, date: NaiveDate) -> Self {
        let end = self.bars.partition_point(|bar| bar.date <= date);
        Self {
            instrument: self.instrument,
            last_completed: date.min(self.last_completed),
            bars: self.bars[..end].to_vec(),
        }
    }
}
