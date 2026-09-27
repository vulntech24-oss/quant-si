//! The live-trading gate (INV-14).
//!
//! A live risk-increasing order requires all of:
//!
//! 1. a build with the `live-orders` Cargo feature,
//! 2. `environment = production`,
//! 3. live trading enabled in configuration,
//! 4. the account's `live_armed` flag (set by a step-up-authenticated owner action),
//! 5. a strategy version at SmallCapital or Full.
//!
//! Risk-reducing live orders need only 1 and 2 (without them no live broker
//! exists at all). Disarming live trading must never trap an open position
//! (INV-02; ADR 0006).

use qd_domain::action::RiskEffect;
use qd_domain::lifecycle::strategy::StrategyStage;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Deployment environment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Environment {
    /// Developer machine or CI.
    #[default]
    Development,
    /// Production deployment.
    Production,
}

/// Live-trading configuration. Disabled by default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LivePolicy {
    /// Deployment environment.
    pub environment: Environment,
    /// Whether live trading is enabled in configuration.
    pub live_trading_enabled: bool,
}

/// Why a live order is not permitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveBlock {
    /// The build lacks the `live-orders` feature.
    #[error("this build cannot place live orders (live-orders feature off)")]
    NotCompiled,
    /// Not a production environment.
    #[error("live orders require environment = production")]
    NotProduction,
    /// Live trading is disabled in configuration.
    #[error("live trading is disabled in configuration")]
    Disabled,
    /// The account is not armed for live trading.
    #[error("the account is not armed for live trading")]
    NotArmed,
    /// The strategy version is not at a live stage.
    #[error("the strategy version is not at SmallCapital or Full")]
    StageNotLive,
}

/// Whether this build was compiled with the `live-orders` feature.
#[must_use]
pub const fn live_orders_compiled() -> bool {
    cfg!(feature = "live-orders")
}

/// Checks the INV-14 conditions for one live order.
pub fn check_live_order(
    policy: &LivePolicy,
    effect: RiskEffect,
    account_live_armed: bool,
    stage: Option<StrategyStage>,
) -> Result<(), LiveBlock> {
    if !live_orders_compiled() {
        return Err(LiveBlock::NotCompiled);
    }
    if policy.environment != Environment::Production {
        return Err(LiveBlock::NotProduction);
    }
    if effect == RiskEffect::Reducing {
        return Ok(());
    }
    if !policy.live_trading_enabled {
        return Err(LiveBlock::Disabled);
    }
    if !account_live_armed {
        return Err(LiveBlock::NotArmed);
    }
    if !stage.is_some_and(StrategyStage::is_live_eligible) {
        return Err(LiveBlock::StageNotLive);
    }
    Ok(())
}
