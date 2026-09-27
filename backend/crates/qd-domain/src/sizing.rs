//! Position sizing (spec §6.5 "Sizing").
//!
//! This module implements the steps that need only the plan and the account:
//!
//! 1. `qty_raw = equity × risk_per_trade × stage_multiplier / risk_net`, in the
//!    account currency using a point-in-time FX rate.
//! 2. Round `qty_raw` down to the quantity step.
//! 3. (Helper) apply a quantity cap. The Risk Gate decides which caps apply.
//! 5. Below the minimum tradable size the result is `TooSmall`, which the
//!    decision core reports as `NoTrade(PositionTooSmall)`.
//!
//! Step 4 (recompute costs at the final quantity, re-check RR and EV) needs the
//! cost model and belongs to the Risk Gate.

use rust_decimal::Decimal;
use serde::Serialize;
use thiserror::Error;

use crate::instrument::InstrumentSpec;
use crate::num::{FxRate, Money, NumError, Quantity, Ratio, checked};

/// Inputs to sizing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SizingInput {
    /// Account equity, in the account currency.
    pub equity: Money,
    /// Fraction of equity risked per trade (0.005 = 0.5%).
    pub risk_per_trade: Ratio,
    /// Stage risk multiplier for this strategy version, in `[0, 1]`.
    pub stage_multiplier: Decimal,
    /// `risk_net` for one unit, in the instrument currency.
    pub risk_net_per_unit: Money,
    /// Point-in-time rate converting the instrument currency into the account currency.
    pub fx: FxRate,
}

/// Why sizing could not be performed. Every case fails closed (INV-06).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SizingError {
    /// Equity is zero or negative.
    #[error("equity must be greater than zero")]
    NonPositiveEquity,
    /// The stage multiplier is outside `[0, 1]`.
    #[error("stage multiplier must be within [0, 1], got {0}")]
    InvalidStageMultiplier(Decimal),
    /// Risk per unit is zero or negative.
    #[error("risk per unit must be greater than zero")]
    NonPositiveRisk,
    /// The FX rate does not convert the instrument currency into the account currency.
    #[error("FX rate does not convert the instrument currency into the account currency")]
    FxMismatch,
    /// A numeric operation failed.
    #[error(transparent)]
    Num(#[from] NumError),
}

/// A sized position.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct PositionSize {
    qty_raw: Decimal,
    quantity: Quantity,
    risk_per_unit: Money,
    risk_budget: Money,
    planned_risk: Money,
}

impl PositionSize {
    /// Unrounded quantity from step 1.
    #[must_use]
    pub const fn qty_raw(&self) -> Decimal {
        self.qty_raw
    }

    /// Final quantity, a valid order size.
    #[must_use]
    pub const fn quantity(&self) -> Quantity {
        self.quantity
    }

    /// `risk_net` per unit in the account currency.
    #[must_use]
    pub const fn risk_per_unit(&self) -> Money {
        self.risk_per_unit
    }

    /// `equity × risk_per_trade × stage_multiplier`.
    #[must_use]
    pub const fn risk_budget(&self) -> Money {
        self.risk_budget
    }

    /// `quantity × risk_per_unit`. Never exceeds the risk budget.
    #[must_use]
    pub const fn planned_risk(&self) -> Money {
        self.planned_risk
    }
}

/// The result of sizing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum SizingOutcome {
    /// A tradable size.
    Sized(PositionSize),
    /// The rounded size is below the instrument's minimum quantity.
    TooSmall {
        /// Quantity after rounding (and caps).
        quantity: Quantity,
        /// Minimum tradable quantity.
        min: Quantity,
    },
}

fn finish(
    qty_raw: Decimal,
    capped: Decimal,
    risk_per_unit: Money,
    risk_budget: Money,
    spec: &InstrumentSpec,
) -> Result<SizingOutcome, SizingError> {
    let quantity = spec.round_quantity_down(capped)?;
    let min = Quantity::new(spec.min_quantity)?;
    if quantity < min {
        return Ok(SizingOutcome::TooSmall { quantity, min });
    }
    let planned_risk = risk_per_unit.checked_scale(quantity.value())?;
    Ok(SizingOutcome::Sized(PositionSize {
        qty_raw,
        quantity,
        risk_per_unit,
        risk_budget,
        planned_risk,
    }))
}

/// Sizing steps 1, 2 and 5.
pub fn size_position(
    input: &SizingInput,
    spec: &InstrumentSpec,
) -> Result<SizingOutcome, SizingError> {
    if input.equity.amount <= Decimal::ZERO {
        return Err(SizingError::NonPositiveEquity);
    }
    if !(Decimal::ZERO..=Decimal::ONE).contains(&input.stage_multiplier) {
        return Err(SizingError::InvalidStageMultiplier(input.stage_multiplier));
    }
    if input.fx.base() != input.risk_net_per_unit.currency
        || input.fx.quote() != input.equity.currency
    {
        return Err(SizingError::FxMismatch);
    }
    let risk_per_unit = input.fx.convert(input.risk_net_per_unit)?;
    if risk_per_unit.amount <= Decimal::ZERO {
        return Err(SizingError::NonPositiveRisk);
    }
    let risk_budget = input
        .equity
        .checked_scale(input.risk_per_trade.value())?
        .checked_scale(input.stage_multiplier)?;
    let qty_raw = checked(risk_budget.amount.checked_div(risk_per_unit.amount))?;
    finish(qty_raw, qty_raw, risk_per_unit, risk_budget, spec)
}

/// Sizing step 3 helper: caps a sized position at `cap` units, re-rounds and re-checks the minimum.
pub fn cap_quantity(
    size: &PositionSize,
    cap: Quantity,
    spec: &InstrumentSpec,
) -> Result<SizingOutcome, SizingError> {
    let capped = size.quantity.min(cap).value();
    finish(
        size.qty_raw,
        capped,
        size.risk_per_unit,
        size.risk_budget,
        spec,
    )
}
