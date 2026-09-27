//! Trend pullback, short side: sell the resumption of a downtrend after a
//! rally to the 20-day average. The mirror image of `trend-pullback-1.0.0`.
//!
//! - Active only in `TrendDown`, and only on instruments that permit an
//!   overnight short (futures, not cash equity); elsewhere it has no setup,
//!   so no NO TRADE decision is journaled for a short that cannot be held.
//! - Setup: the last bar's high came within `pullback_band_atr` ATRs of the
//!   20-day average, and its close stayed below that average.
//! - Entry: stop-limit `entry_offset_ticks` below the setup bar's low.
//! - Stop: `stop_atr` ATRs above the entry. Target: `target_r` × risk below it.
//! - Time exit: `max_holding_days`. Invalidation: a close above the 50-day
//!   average, or the regime leaving `TrendDown`.
//!
//! Logic version 1.0.0; its parameters are part of the version.

use qd_domain::action::EntryAction;
use qd_domain::num::Price;
use qd_domain::plan::{EntryOrderType, InvalidationRule, TradePlanInput};
use qd_domain::proposal::Grade;
use rust_decimal::Decimal;
use serde::Serialize;

use crate::regime::Regime;
use crate::rules::{pct, reason};
use crate::strategy::{SetupCandidate, Strategy, StrategyInput, StrategyOutput};

/// Parameters, fixed for the logic version.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct TrendPullbackShortParams {
    /// How close (in ATRs below the 20-day average) the high must come.
    pub pullback_band_atr: Decimal,
    /// Stop distance above the entry, in ATRs.
    pub stop_atr: Decimal,
    /// Target distance below the entry, in multiples of the risk.
    pub target_r: Decimal,
    /// Maximum holding period in trading days.
    pub max_holding_days: u16,
    /// Ticks below the setup bar's low for the entry trigger.
    pub entry_offset_ticks: u32,
    /// ATR fraction of price above which the grade drops.
    pub elevated_atr_pct: Decimal,
    /// Minimum net reward-to-risk after costs and slippage.
    pub rr_floor: Decimal,
}

impl TrendPullbackShortParams {
    /// The parameters of logic version 1.0.0 (those of the long version).
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

/// The short-side trend-pullback strategy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrendPullbackShort {
    params: TrendPullbackShortParams,
}

impl TrendPullbackShort {
    /// Logic version of this implementation with [`TrendPullbackShortParams::V1`].
    pub const LOGIC_VERSION: &'static str = "trend-pullback-short-1.0.0";

    /// Version 1.0.0.
    #[must_use]
    pub const fn v1() -> Self {
        Self {
            params: TrendPullbackShortParams::V1,
        }
    }

    /// The same logic with other parameters, for research only.
    #[must_use]
    pub const fn with_params(params: TrendPullbackShortParams) -> Self {
        Self { params }
    }

    /// Parameters.
    #[must_use]
    pub const fn params(&self) -> &TrendPullbackShortParams {
        &self.params
    }

    fn candidate(&self, input: &StrategyInput<'_>) -> Option<SetupCandidate> {
        let p = &self.params;
        let f = input.features;
        let band = f
            .sma20
            .checked_sub(p.pullback_band_atr.checked_mul(f.atr14)?)?;
        if !(f.high >= band && f.close < f.sma20) {
            return None;
        }
        let offset = input
            .spec
            .tick_size
            .checked_mul(Decimal::from(p.entry_offset_ticks))?;
        let entry = f.low.checked_sub(offset)?;
        let risk = p.stop_atr.checked_mul(f.atr14)?;
        let stop = entry.checked_add(risk)?;
        let target = entry.checked_sub(p.target_r.checked_mul(risk)?)?;
        if target <= Decimal::ZERO {
            return None;
        }
        let invalidation = match Price::new(f.sma50) {
            Ok(level) => vec![
                InvalidationRule::CloseBeyond { level },
                InvalidationRule::RegimeChange,
            ],
            Err(_) => vec![InvalidationRule::RegimeChange],
        };
        let momentum_down = f.roc20 < Decimal::ZERO;
        let calm = f.atr_pct <= p.elevated_atr_pct;
        let grade = match (momentum_down, calm) {
            (true, true) => Grade::A,
            (true, false) => Grade::B,
            (false, _) => Grade::C,
        };
        let against = if !momentum_down {
            format!(
                "20-day momentum is not negative ({}%): the rally may be a reversal.",
                pct(f.roc20)?
            )
        } else {
            format!(
                "Shorts can gap against you overnight; rallies in downtrends often reach the 50-day average ({}).",
                f.sma50.round_dp(2)
            )
        };
        Some(SetupCandidate {
            plan: TradePlanInput {
                action: EntryAction::OpenShort,
                entry_type: EntryOrderType::StopLimit,
                entry,
                stop,
                target,
                max_holding_days: p.max_holding_days,
                invalidation,
            },
            setup_type: "rally_in_downtrend".to_owned(),
            grade,
            reasons: vec![
                reason(
                    "close_vs_sma200_pct",
                    pct(f.close.checked_div(f.sma200)?.checked_sub(Decimal::ONE)?)?,
                    true,
                ),
                reason(
                    "rally_high_vs_sma20_atr",
                    f.high
                        .checked_sub(f.sma20)?
                        .checked_div(f.atr14)?
                        .round_dp(2),
                    true,
                ),
                reason("roc20_pct", pct(f.roc20)?, momentum_down),
                reason("atr_pct", pct(f.atr_pct)?, calm),
            ],
            strongest_argument_against: against,
        })
    }
}

impl Strategy for TrendPullbackShort {
    fn name(&self) -> &str {
        "Trend pullback (short)"
    }

    fn logic_version(&self) -> &str {
        Self::LOGIC_VERSION
    }

    fn evaluate(&self, input: &StrategyInput<'_>) -> StrategyOutput {
        if input.regime != Regime::TrendDown {
            return StrategyOutput::Inactive {
                regime: input.regime,
            };
        }
        if !input.spec.capabilities.can_short_overnight {
            return StrategyOutput::NoSetup;
        }
        self.candidate(input).map_or(StrategyOutput::NoSetup, |c| {
            StrategyOutput::Setup(Box::new(c))
        })
    }
}
