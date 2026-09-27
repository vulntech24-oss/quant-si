//! Deterministic regime classifier (spec §5.1).
//!
//! Rules, checked in order:
//!
//! 1. `atr_pct ≥ high_volatility_atr_pct` → `HighVolatility`
//! 2. close above the 200-day average, 50-day above 200-day, and the 200-day
//!    average not falling over 20 bars → `TrendUp`
//! 3. the mirror image → `TrendDown`
//! 4. otherwise → `Range`
//!
//! Thresholds belong to the classifier version; changing one is a new version.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::features::FeatureSet;

/// Market regime of one instrument.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Regime {
    /// Established uptrend.
    TrendUp,
    /// Established downtrend.
    TrendDown,
    /// No clear trend.
    Range,
    /// Volatility too high for defined-risk swing trades.
    HighVolatility,
}

impl Regime {
    /// Stable name, used in no-trade reasons and the journal.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::TrendUp => "trend_up",
            Self::TrendDown => "trend_down",
            Self::Range => "range",
            Self::HighVolatility => "high_volatility",
        }
    }
}

/// A versioned regime classifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct RegimeClassifier {
    /// Classifier version.
    pub version: &'static str,
    /// ATR as a fraction of price at or above which the regime is `HighVolatility`.
    pub high_volatility_atr_pct: Decimal,
}

impl RegimeClassifier {
    /// Version 1: high volatility at an ATR of 4% of price.
    pub const V1: Self = Self {
        version: "regime-v1",
        high_volatility_atr_pct: Decimal::from_parts(4, 0, 0, false, 2),
    };

    /// Classifies the regime from a feature set.
    #[must_use]
    pub fn classify(&self, f: &FeatureSet) -> Regime {
        if f.atr_pct >= self.high_volatility_atr_pct {
            Regime::HighVolatility
        } else if f.close > f.sma200 && f.sma50 > f.sma200 && f.sma200 >= f.sma200_prev {
            Regime::TrendUp
        } else if f.close < f.sma200 && f.sma50 < f.sma200 && f.sma200 <= f.sma200_prev {
            Regime::TrendDown
        } else {
            Regime::Range
        }
    }
}
