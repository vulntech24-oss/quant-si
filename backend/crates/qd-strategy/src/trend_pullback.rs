//! Trend pullback (long only): buy the resumption of an uptrend after a pullback
//! to the 20-day average.
//!
//! - Active only in `TrendUp`.
//! - Setup: the last completed bar's low came within `pullback_band_atr` ATRs
//!   of the 20-day average, and its close held above that average.
//! - Entry: stop-limit `entry_offset_ticks` above the setup bar's high, so the
//!   trade only starts if the uptrend resumes.
//! - Stop: `stop_atr` ATRs below the entry. Target: `target_r` times the risk above it.
//! - Time exit: `max_holding_days` trading days. Invalidation: a close below the
//!   50-day average, or the regime leaving `TrendUp`.
//!
//! The 2.5R gross target leaves room for costs and slippage above the 1.5 net
//! RR floor. This is logic version 1.0.0. Its parameters are part of the version.

use qd_domain::action::EntryAction;
use qd_domain::num::Price;
use qd_domain::plan::{EntryOrderType, InvalidationRule, TradePlanInput};
use qd_domain::proposal::{FactorValue, Grade, Reason, ReasonDirection};
use rust_decimal::Decimal;
use serde::Serialize;

use crate::features::FeatureSet;
use crate::regime::Regime;
use crate::strategy::{SetupCandidate, Strategy, StrategyInput, StrategyOutput};

/// Parameters, fixed for the logic version.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct TrendPullbackParams {
    /// How close (in ATRs above the 20-day average) the low must come.
    pub pullback_band_atr: Decimal,
    /// Stop distance below the entry, in ATRs.
    pub stop_atr: Decimal,
    /// Target distance above the entry, in multiples of the risk.
    pub target_r: Decimal,
    /// Maximum holding period in trading days.
    pub max_holding_days: u16,
    /// Ticks above the setup bar's high for the entry trigger.
    pub entry_offset_ticks: u32,
    /// ATR fraction of price above which the grade drops and volatility argues against.
    pub elevated_atr_pct: Decimal,
    /// Minimum net reward-to-risk after costs and slippage (spec §4 "RR floor: set
    /// per strategy"). Below it the Risk Gate returns NO TRADE.
    pub rr_floor: Decimal,
}

impl TrendPullbackParams {
    /// The parameters of logic version 1.0.0.
    pub const V1: Self = Self {
        pullback_band_atr: Decimal::from_parts(5, 0, 0, false, 1),
        stop_atr: Decimal::from_parts(2, 0, 0, false, 0),
        target_r: Decimal::from_parts(25, 0, 0, false, 1),
        max_holding_days: 15,
        entry_offset_ticks: 1,
        elevated_atr_pct: Decimal::from_parts(3, 0, 0, false, 2),
        rr_floor: Decimal::from_parts(15, 0, 0, false, 1),
    };
}

/// The trend-pullback strategy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrendPullback {
    params: TrendPullbackParams,
}

impl TrendPullback {
    /// Logic version of this implementation with [`TrendPullbackParams::V1`].
    pub const LOGIC_VERSION: &'static str = "trend-pullback-1.0.0";

    /// Version 1.0.0.
    #[must_use]
    pub const fn v1() -> Self {
        Self {
            params: TrendPullbackParams::V1,
        }
    }

    /// Parameters.
    #[must_use]
    pub const fn params(&self) -> &TrendPullbackParams {
        &self.params
    }

    fn candidate(&self, input: &StrategyInput<'_>) -> Option<SetupCandidate> {
        let p = &self.params;
        let f = input.features;
        let band = f
            .sma20
            .checked_add(p.pullback_band_atr.checked_mul(f.atr14)?)?;
        let touched = f.low <= band;
        let held = f.close > f.sma20;
        if !(touched && held) {
            return None;
        }
        let offset = input
            .spec
            .tick_size
            .checked_mul(Decimal::from(p.entry_offset_ticks))?;
        let entry = f.high.checked_add(offset)?;
        let risk = p.stop_atr.checked_mul(f.atr14)?;
        let stop = entry.checked_sub(risk)?;
        let target = entry.checked_add(p.target_r.checked_mul(risk)?)?;
        let invalidation = match Price::new(f.sma50) {
            Ok(level) => vec![
                InvalidationRule::CloseBeyond { level },
                InvalidationRule::RegimeChange,
            ],
            Err(_) => vec![InvalidationRule::RegimeChange],
        };
        Some(SetupCandidate {
            plan: TradePlanInput {
                action: EntryAction::OpenLong,
                entry_type: EntryOrderType::StopLimit,
                entry,
                stop,
                target,
                max_holding_days: p.max_holding_days,
                invalidation,
            },
            setup_type: "pullback_in_uptrend".to_owned(),
            grade: self.grade(f),
            reasons: self.reasons(f)?,
            strongest_argument_against: self.argument_against(f, entry)?,
        })
    }

    fn grade(&self, f: &FeatureSet) -> Grade {
        let momentum_up = f.roc20 > Decimal::ZERO;
        let calm = f.atr_pct <= self.params.elevated_atr_pct;
        match (momentum_up, calm) {
            (true, true) => Grade::A,
            (true, false) => Grade::B,
            (false, _) => Grade::C,
        }
    }

    fn reasons(&self, f: &FeatureSet) -> Option<Vec<Reason>> {
        let pct = |value: Decimal| {
            value
                .checked_mul(Decimal::ONE_HUNDRED)
                .map(|v| v.round_dp(2))
        };
        let direction = |supports: bool| {
            if supports {
                ReasonDirection::Supports
            } else {
                ReasonDirection::Opposes
            }
        };
        let close_vs_200 = f.close.checked_div(f.sma200)?.checked_sub(Decimal::ONE)?;
        let sma50_vs_200 = f.sma50.checked_div(f.sma200)?.checked_sub(Decimal::ONE)?;
        let pullback_atr = f
            .low
            .checked_sub(f.sma20)?
            .checked_div(f.atr14)?
            .round_dp(2);
        Some(vec![
            Reason {
                factor: "close_vs_sma200_pct".to_owned(),
                value: FactorValue::Number(pct(close_vs_200)?),
                direction: ReasonDirection::Supports,
            },
            Reason {
                factor: "sma50_vs_sma200_pct".to_owned(),
                value: FactorValue::Number(pct(sma50_vs_200)?),
                direction: ReasonDirection::Supports,
            },
            Reason {
                factor: "pullback_low_vs_sma20_atr".to_owned(),
                value: FactorValue::Number(pullback_atr),
                direction: ReasonDirection::Supports,
            },
            Reason {
                factor: "roc20_pct".to_owned(),
                value: FactorValue::Number(pct(f.roc20)?),
                direction: direction(f.roc20 > Decimal::ZERO),
            },
            Reason {
                factor: "atr_pct".to_owned(),
                value: FactorValue::Number(pct(f.atr_pct)?),
                direction: direction(f.atr_pct <= self.params.elevated_atr_pct),
            },
        ])
    }

    fn argument_against(&self, f: &FeatureSet, entry: Decimal) -> Option<String> {
        let pct = |value: Decimal| {
            value
                .checked_mul(Decimal::ONE_HUNDRED)
                .map(|v| v.round_dp(2))
        };
        Some(if f.roc20 <= Decimal::ZERO {
            format!(
                "20-day momentum is not positive ({}%): the pullback may be the start of a reversal.",
                pct(f.roc20)?
            )
        } else if f.atr_pct > self.params.elevated_atr_pct {
            format!(
                "Volatility is elevated (ATR {}% of price), so the stop is wide and gaps are likely.",
                pct(f.atr_pct)?
            )
        } else {
            format!(
                "The entry only triggers above {}; pullbacks in uptrends often deepen to the 50-day average ({}).",
                entry.round_dp(2),
                f.sma50.round_dp(2)
            )
        })
    }
}

impl Strategy for TrendPullback {
    fn name(&self) -> &str {
        "Trend pullback"
    }

    fn logic_version(&self) -> &str {
        Self::LOGIC_VERSION
    }

    fn evaluate(&self, input: &StrategyInput<'_>) -> StrategyOutput {
        if input.regime != Regime::TrendUp {
            return StrategyOutput::Inactive {
                regime: input.regime,
            };
        }
        // `None` means no setup, or arithmetic overflow on absurd inputs; both
        // fail closed as "no setup".
        self.candidate(input)
            .map_or(StrategyOutput::NoSetup, |candidate| {
                StrategyOutput::Setup(Box::new(candidate))
            })
    }
}
