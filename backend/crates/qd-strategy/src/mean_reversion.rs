//! Mean reversion (long only): buy a bounce from a stretched low inside a
//! range, targeting the 20-day average.
//!
//! - Active only in `Range`.
//! - Setup: the last close is at least `stretch_atr` ATRs below the 20-day
//!   average, but still above the prior 20-day low (a stretch, not a
//!   breakdown).
//! - Entry: stop-limit `entry_offset_ticks` above the setup bar's high, so
//!   the trade only starts once price turns up.
//! - Stop: `stop_atr` ATRs below the entry. Target: the 20-day average. If
//!   that leaves too little reward, the Risk Gate's RR floor says NO TRADE.
//! - Time exit: `max_holding_days`. Invalidation: a close below the prior
//!   20-day low, or the regime leaving `Range`.
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
pub struct MeanReversionParams {
    /// How far below the 20-day average (ATRs) the close must be.
    pub stretch_atr: Decimal,
    /// Stop distance below the entry, in ATRs.
    pub stop_atr: Decimal,
    /// Maximum holding period in trading days.
    pub max_holding_days: u16,
    /// Ticks above the setup bar's high for the entry trigger.
    pub entry_offset_ticks: u32,
    /// Minimum net reward-to-risk after costs and slippage.
    pub rr_floor: Decimal,
}

impl MeanReversionParams {
    /// The parameters of logic version 1.0.0.
    pub const V1: Self = Self {
        stretch_atr: Decimal::from_parts(15, 0, 0, false, 1),
        stop_atr: Decimal::from_parts(1, 0, 0, false, 0),
        max_holding_days: 10,
        entry_offset_ticks: 1,
        rr_floor: Decimal::from_parts(15, 0, 0, false, 1),
    };
}

/// The mean-reversion strategy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MeanReversion {
    params: MeanReversionParams,
}

impl MeanReversion {
    /// Logic version of this implementation with [`MeanReversionParams::V1`].
    pub const LOGIC_VERSION: &'static str = "mean-reversion-1.0.0";

    /// Version 1.0.0.
    #[must_use]
    pub const fn v1() -> Self {
        Self {
            params: MeanReversionParams::V1,
        }
    }

    /// The same logic with other parameters, for research only.
    #[must_use]
    pub const fn with_params(params: MeanReversionParams) -> Self {
        Self { params }
    }

    /// Parameters.
    #[must_use]
    pub const fn params(&self) -> &MeanReversionParams {
        &self.params
    }

    fn candidate(&self, input: &StrategyInput<'_>) -> Option<SetupCandidate> {
        let p = &self.params;
        let f = input.features;
        let stretch = f.sma20.checked_sub(f.close)?.checked_div(f.atr14)?;
        if stretch < p.stretch_atr || f.close <= f.prior_low20 {
            return None;
        }
        let offset = input
            .spec
            .tick_size
            .checked_mul(Decimal::from(p.entry_offset_ticks))?;
        let entry = f.high.checked_add(offset)?;
        let target = f.sma20;
        if target <= entry {
            return None;
        }
        let stop = entry.checked_sub(p.stop_atr.checked_mul(f.atr14)?)?;
        let mut invalidation = vec![InvalidationRule::RegimeChange];
        if let Ok(level) = Price::new(f.prior_low20) {
            invalidation.insert(0, InvalidationRule::CloseBeyond { level });
        }
        let room = f.close.checked_sub(f.prior_low20)?.checked_div(f.atr14)?;
        let grade = if stretch >= p.stretch_atr.checked_add(Decimal::ONE)? {
            Grade::B
        } else if room >= Decimal::ONE {
            Grade::A
        } else {
            Grade::C
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
            setup_type: "stretch_below_mean_in_range".to_owned(),
            grade,
            reasons: vec![
                reason("stretch_below_sma20_atr", stretch.round_dp(2), true),
                reason(
                    "room_above_prior_low20_atr",
                    room.round_dp(2),
                    room >= Decimal::ONE,
                ),
                reason("roc20_pct", pct(f.roc20)?, f.roc20 > Decimal::ZERO),
            ],
            strongest_argument_against: format!(
                "Only {} ATR above the prior 20-day low ({}): a stretch can become a breakdown, and ranges end.",
                room.round_dp(2),
                f.prior_low20.round_dp(2)
            ),
        })
    }
}

impl Strategy for MeanReversion {
    fn name(&self) -> &str {
        "Mean reversion"
    }

    fn logic_version(&self) -> &str {
        Self::LOGIC_VERSION
    }

    fn evaluate(&self, input: &StrategyInput<'_>) -> StrategyOutput {
        if input.regime != Regime::Range {
            return StrategyOutput::Inactive {
                regime: input.regime,
            };
        }
        self.candidate(input).map_or(StrategyOutput::NoSetup, |c| {
            StrategyOutput::Setup(Box::new(c))
        })
    }
}
