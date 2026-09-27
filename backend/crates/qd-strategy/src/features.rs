//! Feature engine (spec §5.1 "feature engine"): indicators over completed daily bars.
//!
//! Everything is Decimal and deterministic, so a replay gives identical values
//! (INV-09). The feature set is versioned; changing a definition or a period
//! means a new [`FEATURE_SET_VERSION`].

use chrono::NaiveDate;
use qd_domain::market::{Bar, BarSeries};
use rust_decimal::Decimal;
use serde::Serialize;
use thiserror::Error;

/// Version of the feature definitions below.
pub const FEATURE_SET_VERSION: &str = "features-v1";

/// Bars needed: the 200-day average plus its 20-day slope lookback.
pub const MIN_BARS: usize = 220;

/// Why features could not be computed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FeatureError {
    /// Not enough completed bars.
    #[error("need {needed} completed bars, have {have}")]
    InsufficientHistory {
        /// Bars needed.
        needed: usize,
        /// Bars available.
        have: usize,
    },
    /// Decimal arithmetic overflowed.
    #[error("decimal arithmetic overflow")]
    Overflow,
}

fn arith(value: Option<Decimal>) -> Result<Decimal, FeatureError> {
    value.ok_or(FeatureError::Overflow)
}

fn tail<T>(values: &[T], len: usize) -> Option<&[T]> {
    values.len().checked_sub(len).map(|start| &values[start..])
}

/// Mean of the last `period` values.
pub fn sma(values: &[Decimal], period: usize) -> Option<Decimal> {
    if period == 0 {
        return None;
    }
    let window = tail(values, period)?;
    let sum = window
        .iter()
        .try_fold(Decimal::ZERO, |acc, v| acc.checked_add(*v))?;
    sum.checked_div(Decimal::from(period))
}

/// True range of each bar; the first bar uses high − low.
pub fn true_ranges(bars: &[Bar]) -> Option<Vec<Decimal>> {
    let mut out = Vec::with_capacity(bars.len());
    let mut previous_close: Option<Decimal> = None;
    for bar in bars {
        let (high, low) = (bar.high().value(), bar.low().value());
        let mut range = high.checked_sub(low)?;
        if let Some(close) = previous_close {
            range = range
                .max(high.checked_sub(close)?.abs())
                .max(low.checked_sub(close)?.abs());
        }
        out.push(range);
        previous_close = Some(bar.close().value());
    }
    Some(out)
}

/// Wilder's average true range over `period` bars: the first value is the mean
/// of the first `period` true ranges, then `(previous × (n − 1) + TR) / n`.
pub fn atr(bars: &[Bar], period: usize) -> Option<Decimal> {
    if period == 0 {
        return None;
    }
    let ranges = true_ranges(bars)?;
    let n = Decimal::from(period);
    let mut value = sma(ranges.get(..period)?, period)?;
    for range in ranges.get(period..)? {
        value = value
            .checked_mul(n.checked_sub(Decimal::ONE)?)?
            .checked_add(*range)?
            .checked_div(n)?;
    }
    Some(value)
}

/// Highest high of the last `period` bars.
pub fn highest_high(bars: &[Bar], period: usize) -> Option<Decimal> {
    tail(bars, period)?.iter().map(|b| b.high().value()).max()
}

/// Lowest low of the last `period` bars.
pub fn lowest_low(bars: &[Bar], period: usize) -> Option<Decimal> {
    tail(bars, period)?.iter().map(|b| b.low().value()).min()
}

/// `last / value_period_bars_ago − 1`.
pub fn rate_of_change(values: &[Decimal], period: usize) -> Option<Decimal> {
    let last = *values.last()?;
    let base = *values.get(values.len().checked_sub(period.checked_add(1)?)?)?;
    last.checked_div(base)?.checked_sub(Decimal::ONE)
}

/// The v1 feature set at the last completed bar.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FeatureSet {
    /// Feature-set version.
    pub version: &'static str,
    /// Date of the last completed bar.
    pub as_of: NaiveDate,
    /// Last bar open.
    pub open: Decimal,
    /// Last bar high.
    pub high: Decimal,
    /// Last bar low.
    pub low: Decimal,
    /// Last bar close.
    pub close: Decimal,
    /// 20-day simple average of closes.
    pub sma20: Decimal,
    /// 50-day simple average of closes.
    pub sma50: Decimal,
    /// 200-day simple average of closes.
    pub sma200: Decimal,
    /// 200-day average as of 20 bars earlier (slope reference).
    pub sma200_prev: Decimal,
    /// 14-day Wilder ATR.
    pub atr14: Decimal,
    /// ATR as a fraction of the close.
    pub atr_pct: Decimal,
    /// Highest high of the 20 bars before the last one.
    pub prior_high20: Decimal,
    /// Lowest low of the 20 bars before the last one.
    pub prior_low20: Decimal,
    /// 20-day rate of change of the close.
    pub roc20: Decimal,
}

impl FeatureSet {
    /// Computes the feature set from the whole series (completed bars only).
    pub fn compute(series: &BarSeries) -> Result<Self, FeatureError> {
        let bars = series.bars();
        if bars.len() < MIN_BARS {
            return Err(FeatureError::InsufficientHistory {
                needed: MIN_BARS,
                have: bars.len(),
            });
        }
        let closes: Vec<Decimal> = bars.iter().map(|b| b.close().value()).collect();
        let last = bars.last().ok_or(FeatureError::InsufficientHistory {
            needed: MIN_BARS,
            have: 0,
        })?;
        let before_last = &bars[..bars.len() - 1];
        let close = last.close().value();
        let atr14 = arith(atr(bars, 14))?;
        Ok(Self {
            version: FEATURE_SET_VERSION,
            as_of: last.date(),
            open: last.open().value(),
            high: last.high().value(),
            low: last.low().value(),
            close,
            sma20: arith(sma(&closes, 20))?,
            sma50: arith(sma(&closes, 50))?,
            sma200: arith(sma(&closes, 200))?,
            sma200_prev: arith(sma(&closes[..closes.len() - 20], 200))?,
            atr14,
            atr_pct: arith(atr14.checked_div(close))?,
            prior_high20: arith(highest_high(before_last, 20))?,
            prior_low20: arith(lowest_low(before_last, 20))?,
            roc20: arith(rate_of_change(&closes, 20))?,
        })
    }
}
