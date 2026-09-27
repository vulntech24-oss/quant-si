//! Risk configuration (spec §4 "Default risk configuration").
//!
//! Values are configuration, never code. [`RiskConfig::new`] validates them so
//! a bad file fails at load time, not in the middle of a decision.

use std::ops::Deref;

use qd_domain::instrument::AssetClass;
use qd_domain::lifecycle::strategy::TradingStage;
use qd_domain::num::Ratio;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Stage risk multipliers applied to live accounts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageMultipliers {
    /// Paper stage. Must be 0: paper versions never trade live (INV-14).
    pub paper: Decimal,
    /// SmallCapital stage.
    pub small_capital: Decimal,
    /// Full stage.
    pub full: Decimal,
}

impl StageMultipliers {
    /// The multiplier for a trading stage.
    #[must_use]
    pub const fn for_stage(&self, stage: TradingStage) -> Decimal {
        match stage {
            TradingStage::Paper => self.paper,
            TradingStage::SmallCapital => self.small_capital,
            TradingStage::Full => self.full,
        }
    }
}

/// Gap shock per asset class: the loss assumed for a position without valid protection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GapShocks {
    /// Stocks and equity ETFs.
    pub equity: Ratio,
    /// Gold and silver.
    pub precious_metal: Ratio,
    /// Crude oil.
    pub energy: Ratio,
    /// Crypto.
    pub crypto: Ratio,
}

impl GapShocks {
    /// The shock for an asset class.
    #[must_use]
    pub const fn for_asset_class(&self, asset_class: AssetClass) -> Ratio {
        match asset_class {
            AssetClass::Equity => self.equity,
            AssetClass::PreciousMetal => self.precious_metal,
            AssetClass::Energy => self.energy,
            AssetClass::Crypto => self.crypto,
        }
    }
}

/// The editable risk configuration. Validate it with [`RiskConfig::new`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskConfigData {
    /// Fraction of equity risked per trade.
    pub risk_per_trade: Ratio,
    /// Cap on total open risk, as a fraction of equity.
    pub max_total_open_risk: Ratio,
    /// Cap on open risk per correlated bucket.
    pub max_bucket_open_risk: Ratio,
    /// Cap on open risk per strategy version.
    pub max_strategy_open_risk: Ratio,
    /// Daily loss limit, as a fraction of start-of-day equity, open P&L included.
    pub daily_loss_limit: Ratio,
    /// Weekly loss limit, as a fraction of start-of-week equity.
    pub weekly_loss_limit: Ratio,
    /// Drawdown from the high-water mark that triggers a hard halt.
    pub hard_halt_drawdown: Ratio,
    /// Losing trades in a row that trigger a cool-off.
    pub cool_off_after_losses: u32,
    /// How long a cool-off lasts, in hours.
    pub cool_off_hours: u32,
    /// Stage multipliers for live accounts.
    pub stage_multipliers: StageMultipliers,
    /// Whether paper and backtest accounts size as if at Full, so results compare with the backtest.
    pub paper_accounts_size_as_full: bool,
    /// Minimum expected value after costs, in R.
    pub min_ev_r: Decimal,
    /// Minimum comparable out-of-sample setups (used by the Decision Engine).
    pub min_evidence: u32,
    /// Gap shocks for unprotected positions.
    pub gap_shocks: GapShocks,
}

/// Why a risk configuration is invalid.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("invalid risk configuration: {0}")]
pub struct RiskConfigError(pub String);

/// A validated risk configuration. Read its fields through `Deref`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RiskConfig(RiskConfigData);

impl Deref for RiskConfig {
    type Target = RiskConfigData;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

fn check(condition: bool, message: &str) -> Result<(), RiskConfigError> {
    if condition {
        Ok(())
    } else {
        Err(RiskConfigError(message.to_owned()))
    }
}

fn is_open_fraction(ratio: Ratio) -> bool {
    ratio.value() > Decimal::ZERO && ratio.value() < Decimal::ONE
}

impl RiskConfig {
    /// Validates the configuration.
    pub fn new(data: RiskConfigData) -> Result<Self, RiskConfigError> {
        for (name, ratio) in [
            ("risk_per_trade", data.risk_per_trade),
            ("max_total_open_risk", data.max_total_open_risk),
            ("max_bucket_open_risk", data.max_bucket_open_risk),
            ("max_strategy_open_risk", data.max_strategy_open_risk),
            ("daily_loss_limit", data.daily_loss_limit),
            ("weekly_loss_limit", data.weekly_loss_limit),
            ("hard_halt_drawdown", data.hard_halt_drawdown),
        ] {
            check(
                is_open_fraction(ratio),
                &format!("{name} must be between 0 and 1 (exclusive)"),
            )?;
        }
        check(
            data.risk_per_trade <= data.max_bucket_open_risk
                && data.risk_per_trade <= data.max_strategy_open_risk,
            "risk_per_trade must not exceed the bucket and strategy caps",
        )?;
        check(
            data.max_bucket_open_risk <= data.max_total_open_risk
                && data.max_strategy_open_risk <= data.max_total_open_risk,
            "bucket and strategy caps must not exceed the total cap",
        )?;
        check(
            data.cool_off_after_losses >= 1 && data.cool_off_hours >= 1,
            "cool-off threshold and duration must be at least 1",
        )?;
        let m = data.stage_multipliers;
        check(
            m.paper.is_zero(),
            "the paper stage multiplier must be 0: paper versions never trade live",
        )?;
        check(
            Decimal::ZERO < m.small_capital && m.small_capital <= m.full && m.full <= Decimal::ONE,
            "stage multipliers must satisfy 0 < small_capital <= full <= 1",
        )?;
        check(
            data.min_ev_r >= Decimal::ZERO,
            "min_ev_r must not be negative",
        )?;
        let g = data.gap_shocks;
        for shock in [g.equity, g.precious_metal, g.energy, g.crypto] {
            check(
                shock.value() > Decimal::ZERO && shock.value() <= Decimal::ONE,
                "gap shocks must be in (0, 1]",
            )?;
        }
        Ok(Self(data))
    }
}
