//! Breakout (long only): buy the continuation of a close above the prior
//! 20-day high.
//!
//! - Active in `TrendUp` and `Range`; inactive in `TrendDown` and
//!   `HighVolatility`.
//! - Setup: the last close is above the highest high of the 20 bars before
//!   it and above the 50-day average, and not more than `max_extension_atr`
//!   ATRs above that high (a late, extended breakout is not chased).
//! - Entry: stop-limit `entry_offset_ticks` above the breakout bar's high.
//! - Stop: `stop_atr` ATRs below the entry. Target: `target_r` × risk above it.
//! - Time exit: `max_holding_days`. Invalidation: a close back below the
//!   20-day average (the breakout failed).
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
pub struct BreakoutParams {
    /// Largest distance (ATRs) of the close above the prior 20-day high.
    pub max_extension_atr: Decimal,
    /// Stop distance below the entry, in ATRs.
    pub stop_atr: Decimal,
    /// Target distance above the entry, in multiples of the risk.
    pub target_r: Decimal,
    /// Maximum holding period in trading days.
    pub max_holding_days: u16,
    /// Ticks above the breakout bar's high for the entry trigger.
    pub entry_offset_ticks: u32,
    /// ATR fraction of price above which the grade drops.
    pub elevated_atr_pct: Decimal,
    /// Minimum net reward-to-risk after costs and slippage.
    pub rr_floor: Decimal,
}

impl BreakoutParams {
    /// The parameters of logic version 1.0.0.
    pub const V1: Self = Self {
        max_extension_atr: Decimal::from_parts(1, 0, 0, false, 0),
        stop_atr: Decimal::from_parts(2, 0, 0, false, 0),
        target_r: Decimal::from_parts(3, 0, 0, false, 0),
        max_holding_days: 20,
        entry_offset_ticks: 1,
        elevated_atr_pct: Decimal::from_parts(3, 0, 0, false, 2),
        rr_floor: Decimal::from_parts(15, 0, 0, false, 1),
    };
}

/// The breakout strategy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Breakout {
    params: BreakoutParams,
}

impl Breakout {
    /// Logic version of this implementation with [`BreakoutParams::V1`].
    pub const LOGIC_VERSION: &'static str = "breakout-1.0.0";

    /// Version 1.0.0.
    #[must_use]
    pub const fn v1() -> Self {
        Self {
            params: BreakoutParams::V1,
        }
    }

    /// The same logic with other parameters, for research only (a
    /// parameter search). Only [`Self::v1`] is in the catalog (INV-10).
    #[must_use]
    pub const fn with_params(params: BreakoutParams) -> Self {
        Self { params }
    }

    /// Parameters.
    #[must_use]
    pub const fn params(&self) -> &BreakoutParams {
        &self.params
    }

    fn candidate(&self, input: &StrategyInput<'_>) -> Option<SetupCandidate> {
        let p = &self.params;
        let f = input.features;
        let broke_out = f.close > f.prior_high20 && f.close > f.sma50;
        let extension = f.close.checked_sub(f.prior_high20)?.checked_div(f.atr14)?;
        if !broke_out || extension > p.max_extension_atr {
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
        let invalidation = Price::new(f.sma20)
            .map(|level| vec![InvalidationRule::CloseBeyond { level }])
            .unwrap_or_default();
        let momentum = f.roc20 > Decimal::ZERO;
        let calm = f.atr_pct <= p.elevated_atr_pct;
        let trend = input.regime == Regime::TrendUp;
        let grade = match (trend && momentum, calm) {
            (true, true) => Grade::A,
            (true, false) | (false, true) => Grade::B,
            (false, false) => Grade::C,
        };
        let against = if input.regime == Regime::Range {
            "Range regime: most breakouts from a range fail and fall back inside it.".to_owned()
        } else if !calm {
            format!(
                "Volatility is elevated (ATR {}% of price), so the stop is wide.",
                pct(f.atr_pct)?
            )
        } else {
            format!(
                "The breakout is {} ATR above the prior high; a pullback to it ({}) is common before any follow-through.",
                extension.round_dp(2),
                f.prior_high20.round_dp(2)
            )
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
            setup_type: "breakout_20d_high".to_owned(),
            grade,
            reasons: vec![
                reason("breakout_extension_atr", extension.round_dp(2), true),
                reason(
                    "close_vs_sma50_pct",
                    pct(f.close.checked_div(f.sma50)?.checked_sub(Decimal::ONE)?)?,
                    true,
                ),
                reason("roc20_pct", pct(f.roc20)?, momentum),
                reason("atr_pct", pct(f.atr_pct)?, calm),
            ],
            strongest_argument_against: against,
        })
    }
}

impl Strategy for Breakout {
    fn name(&self) -> &str {
        "Breakout"
    }

    fn logic_version(&self) -> &str {
        Self::LOGIC_VERSION
    }

    fn evaluate(&self, input: &StrategyInput<'_>) -> StrategyOutput {
        if !matches!(input.regime, Regime::TrendUp | Regime::Range) {
            return StrategyOutput::Inactive {
                regime: input.regime,
            };
        }
        self.candidate(input).map_or(StrategyOutput::NoSetup, |c| {
            StrategyOutput::Setup(Box::new(c))
        })
    }
}
