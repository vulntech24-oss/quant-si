//! Trade plans: entry, stop, target, time exit and invalidation (spec §6.4).
//!
//! Levels are rounded to the instrument's tick size when the plan is built,
//! before any economics are computed (INV-13). Rounding is conservative for the
//! plan's economics: the entry moves against the trade, the stop moves away
//! from the entry (more risk) and the target moves toward it (less reward).

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::action::{EntryAction, Side};
use crate::instrument::{InstrumentSpec, Rounding};
use crate::num::Price;

/// A level in a trade plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanLevel {
    /// Entry price.
    Entry,
    /// Stop loss.
    Stop,
    /// Target.
    Target,
}

/// Why a trade plan or its economics are invalid.
#[derive(Clone, Debug, PartialEq, Eq, Error, Serialize, Deserialize)]
#[serde(tag = "defect", rename_all = "snake_case")]
pub enum PlanDefect {
    /// A level is zero or negative after tick rounding.
    #[error("{level:?} must be greater than zero after tick rounding")]
    NonPositiveLevel {
        /// The level.
        level: PlanLevel,
    },
    /// The stop is not on the losing side of the entry.
    #[error("stop is on the wrong side of the entry")]
    StopOnWrongSide,
    /// The target is not on the winning side of the entry.
    #[error("target is on the wrong side of the entry")]
    TargetOnWrongSide,
    /// The maximum holding period is zero.
    #[error("maximum holding period must be at least one trading day")]
    ZeroHoldingPeriod,
    /// Costs consume the whole reward.
    #[error("net reward is not positive after costs")]
    NonPositiveNetReward,
    /// The cost estimate is malformed.
    #[error("invalid cost estimate: {detail}")]
    InvalidCosts {
        /// What is wrong.
        detail: String,
    },
    /// The slippage assumption is malformed.
    #[error("invalid slippage assumption: {detail}")]
    InvalidSlippage {
        /// What is wrong.
        detail: String,
    },
    /// Outcome probabilities are out of range or do not sum to one.
    #[error("invalid outcome probabilities: {detail}")]
    InvalidProbabilities {
        /// What is wrong.
        detail: String,
    },
    /// An amount is not in the instrument's currency.
    #[error("amount is not in the instrument currency")]
    CurrencyMismatch,
    /// Decimal arithmetic overflowed.
    #[error("decimal arithmetic overflow")]
    ArithmeticOverflow,
}

pub(crate) fn overflow(value: Option<Decimal>) -> Result<Decimal, PlanDefect> {
    value.ok_or(PlanDefect::ArithmeticOverflow)
}

/// How the entry is placed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryOrderType {
    /// Limit order at the entry price.
    Limit,
    /// Stop-limit order triggered at the entry price (breakouts).
    StopLimit,
    /// Market order; the entry price is the reference used for economics.
    Market,
}

/// The entry price and order type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryOrder {
    /// Order type.
    pub order_type: EntryOrderType,
    /// Entry price, tick-aligned.
    pub price: Price,
}

/// A rule that invalidates the setup before the stop is hit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "rule", rename_all = "snake_case")]
pub enum InvalidationRule {
    /// A daily close beyond this level (below it for longs, above it for shorts).
    CloseBeyond {
        /// The level.
        level: Price,
    },
    /// The regime classifier moves to a regime where the strategy is inactive.
    RegimeChange,
    /// A strategy-specific rule.
    Custom {
        /// Stable code.
        code: String,
        /// Human-readable description.
        description: String,
    },
}

/// Unvalidated plan levels, as a strategy produces them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TradePlanInput {
    /// OpenLong or OpenShort.
    pub action: EntryAction,
    /// How the entry is placed.
    pub entry_type: EntryOrderType,
    /// Raw entry price.
    pub entry: Decimal,
    /// Raw stop loss.
    pub stop: Decimal,
    /// Raw target.
    pub target: Decimal,
    /// Maximum holding period in trading days.
    pub max_holding_days: u16,
    /// Invalidation rules.
    pub invalidation: Vec<InvalidationRule>,
}

/// A validated, tick-rounded trade plan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TradePlan {
    action: EntryAction,
    entry: EntryOrder,
    stop: Price,
    target: Price,
    max_holding_days: u16,
    invalidation: Vec<InvalidationRule>,
    risk_points: Decimal,
    reward_points: Decimal,
}

fn round_level(
    spec: &InstrumentSpec,
    value: Decimal,
    direction: Rounding,
    level: PlanLevel,
) -> Result<Price, PlanDefect> {
    spec.round_price(value, direction)
        .map_err(|_| PlanDefect::NonPositiveLevel { level })
}

impl TradePlan {
    /// Rounds the levels to the tick size and validates their geometry.
    pub fn new(input: TradePlanInput, spec: &InstrumentSpec) -> Result<Self, PlanDefect> {
        if input.max_holding_days == 0 {
            return Err(PlanDefect::ZeroHoldingPeriod);
        }
        let side = input.action.side();
        let (entry_dir, stop_dir, target_dir) = match side {
            Side::Long => (Rounding::Up, Rounding::Down, Rounding::Down),
            Side::Short => (Rounding::Down, Rounding::Up, Rounding::Up),
        };
        let entry = round_level(spec, input.entry, entry_dir, PlanLevel::Entry)?;
        let stop = round_level(spec, input.stop, stop_dir, PlanLevel::Stop)?;
        let target = round_level(spec, input.target, target_dir, PlanLevel::Target)?;

        let (e, s, t) = (entry.value(), stop.value(), target.value());
        let (risk_points, reward_points) = match side {
            Side::Long => (overflow(e.checked_sub(s))?, overflow(t.checked_sub(e))?),
            Side::Short => (overflow(s.checked_sub(e))?, overflow(e.checked_sub(t))?),
        };
        if risk_points <= Decimal::ZERO {
            return Err(PlanDefect::StopOnWrongSide);
        }
        if reward_points <= Decimal::ZERO {
            return Err(PlanDefect::TargetOnWrongSide);
        }
        Ok(Self {
            action: input.action,
            entry: EntryOrder {
                order_type: input.entry_type,
                price: entry,
            },
            stop,
            target,
            max_holding_days: input.max_holding_days,
            invalidation: input.invalidation,
            risk_points,
            reward_points,
        })
    }

    /// OpenLong or OpenShort.
    #[must_use]
    pub const fn action(&self) -> EntryAction {
        self.action
    }

    /// Entry price and order type.
    #[must_use]
    pub const fn entry(&self) -> EntryOrder {
        self.entry
    }

    /// Stop loss, tick-aligned.
    #[must_use]
    pub const fn stop(&self) -> Price {
        self.stop
    }

    /// Target, tick-aligned.
    #[must_use]
    pub const fn target(&self) -> Price {
        self.target
    }

    /// Maximum holding period in trading days (the time exit).
    #[must_use]
    pub const fn max_holding_days(&self) -> u16 {
        self.max_holding_days
    }

    /// Invalidation rules.
    #[must_use]
    pub fn invalidation(&self) -> &[InvalidationRule] {
        &self.invalidation
    }

    /// Distance from entry to stop in price points. Always positive.
    #[must_use]
    pub const fn risk_points(&self) -> Decimal {
        self.risk_points
    }

    /// Distance from entry to target in price points. Always positive.
    #[must_use]
    pub const fn reward_points(&self) -> Decimal {
        self.reward_points
    }
}
