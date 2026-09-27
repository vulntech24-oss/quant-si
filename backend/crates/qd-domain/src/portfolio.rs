//! Open risk, P&L, drawdown and R-multiple (spec §6.5).
//!
//! Amounts use the contract multiplier and a point-in-time FX rate so that
//! open risk is always in the account currency (ADR 0003).

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::action::Side;
use crate::num::{FxRate, Money, NumError, Price, Quantity, Ratio, checked};

/// Whether a position has valid protection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "protection", rename_all = "snake_case")]
pub enum Protection {
    /// A working protective stop at this level.
    Valid {
        /// Stop level.
        stop: Price,
    },
    /// No valid protection. The position counts at a gap shock and raises an alert.
    Missing,
}

/// What open risk needs to know about one position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PositionExposure {
    /// Long or short.
    pub side: Side,
    /// Open quantity.
    pub quantity: Quantity,
    /// Current mark price.
    pub mark: Price,
    /// Protection state.
    pub protection: Protection,
    /// Contract multiplier from the instrument spec.
    pub multiplier: Decimal,
    /// Point-in-time rate from the instrument currency into the account currency.
    pub fx: FxRate,
    /// Gap shock for the asset class, applied to unprotected positions.
    pub gap_shock: Ratio,
}

/// Open risk of one position, in the account currency.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct OpenRisk {
    /// Risk amount.
    pub amount: Money,
    /// True when the position had no valid protection and was counted at a gap shock.
    /// The caller must raise an alert.
    pub unprotected: bool,
}

/// Why a portfolio calculation failed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PortfolioError {
    /// The multiplier is zero or negative.
    #[error("contract multiplier must be greater than zero")]
    NonPositiveMultiplier,
    /// The high-water mark is zero or negative.
    #[error("high-water mark must be greater than zero")]
    NonPositiveHighWaterMark,
    /// Equity is above the high-water mark, so the mark is stale (INV-06).
    #[error("equity is above the high-water mark; update the mark first")]
    EquityAboveHighWaterMark,
    /// Risk at entry was zero.
    #[error("planned risk at entry must be greater than zero")]
    ZeroPlannedRisk,
    /// A numeric operation failed.
    #[error(transparent)]
    Num(#[from] NumError),
}

/// Open risk of one position:
/// protected → `max(0, qty × multiplier × (mark − stop))` for longs, mirrored for shorts;
/// unprotected → `qty × multiplier × mark × gap_shock`. Converted into the account currency.
pub fn position_open_risk(position: &PositionExposure) -> Result<OpenRisk, PortfolioError> {
    if position.multiplier <= Decimal::ZERO {
        return Err(PortfolioError::NonPositiveMultiplier);
    }
    let units = checked(position.quantity.value().checked_mul(position.multiplier))?;
    let mark = position.mark.value();
    let (per_unit, unprotected) = match position.protection {
        Protection::Valid { stop } => {
            let distance = match position.side {
                Side::Long => checked(mark.checked_sub(stop.value()))?,
                Side::Short => checked(stop.value().checked_sub(mark))?,
            };
            (distance.max(Decimal::ZERO), false)
        }
        Protection::Missing => (checked(mark.checked_mul(position.gap_shock.value()))?, true),
    };
    let amount = Money::new(checked(units.checked_mul(per_unit))?, position.fx.base());
    Ok(OpenRisk {
        amount: position.fx.convert(amount)?,
        unprotected,
    })
}

/// Planned risk of a working entry order: `quantity × risk_net_per_unit`, in the account currency.
pub fn working_entry_risk(
    quantity: Quantity,
    risk_net_per_unit: Money,
    fx: &FxRate,
) -> Result<Money, NumError> {
    fx.convert(risk_net_per_unit.checked_scale(quantity.value())?)
}

/// Sums risk amounts that must all be in `currency`.
pub fn total_open_risk(
    amounts: impl IntoIterator<Item = Money>,
    currency: crate::num::Currency,
) -> Result<Money, NumError> {
    amounts
        .into_iter()
        .try_fold(Money::zero(currency), Money::checked_add)
}

/// Daily P&L: `equity_now − equity_at_day_start`.
pub fn daily_pnl(equity_now: Money, equity_at_day_start: Money) -> Result<Money, NumError> {
    equity_now.checked_sub(equity_at_day_start)
}

/// The high-water mark after observing `equity`.
pub fn update_high_water_mark(high_water_mark: Money, equity: Money) -> Result<Money, NumError> {
    let equity = equity.ensure_currency(high_water_mark.currency)?;
    Ok(if equity.amount > high_water_mark.amount {
        equity
    } else {
        high_water_mark
    })
}

/// Drawdown: `(high_water_mark − equity) / high_water_mark`.
pub fn drawdown(high_water_mark: Money, equity: Money) -> Result<Ratio, PortfolioError> {
    let equity = equity.ensure_currency(high_water_mark.currency)?;
    if high_water_mark.amount <= Decimal::ZERO {
        return Err(PortfolioError::NonPositiveHighWaterMark);
    }
    if equity.amount > high_water_mark.amount {
        return Err(PortfolioError::EquityAboveHighWaterMark);
    }
    let fall = checked(high_water_mark.amount.checked_sub(equity.amount))?;
    Ok(Ratio::new(checked(
        fall.checked_div(high_water_mark.amount),
    )?)?)
}

/// Trade R-multiple: `net_pnl / (risk_net_per_unit × quantity)`, using the risk planned at entry.
pub fn r_multiple(
    net_pnl: Money,
    risk_net_per_unit: Money,
    quantity: Quantity,
) -> Result<Decimal, PortfolioError> {
    let net_pnl = net_pnl.ensure_currency(risk_net_per_unit.currency)?;
    let planned = checked(risk_net_per_unit.amount.checked_mul(quantity.value()))?;
    if planned <= Decimal::ZERO {
        return Err(PortfolioError::ZeroPlannedRisk);
    }
    Ok(checked(net_pnl.amount.checked_div(planned))?)
}
