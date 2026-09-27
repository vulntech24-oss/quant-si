//! Decision outcomes and the typed reasons behind them (spec §6.3).
//!
//! `NO TRADE` is a first-class, frequent outcome. Every no-trade decision
//! carries a typed reason with its data so the UI can show the post-risk
//! verdict and why (INV-17).

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::action::{EntryAction, ExitAction};
use crate::halt::HaltBlock;
use crate::ids::AiReviewId;
use crate::instrument::CorrelationBucket;
use crate::lifecycle::strategy::StrategyStage;
use crate::num::{Price, Quantity, Ratio};
use crate::plan::PlanDefect;

/// The result of the decision pipeline for one opportunity or position.
///
/// `Enter` can only hold an opening action and `Exit` only a closing one, so an
/// ambiguous outcome cannot be constructed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum DecisionOutcome {
    /// Open a position.
    Enter {
        /// OpenLong or OpenShort.
        action: EntryAction,
    },
    /// Close a position.
    Exit {
        /// CloseLong or CloseShort.
        action: ExitAction,
        /// Why the position is being closed.
        reason: ExitReason,
    },
    /// Keep an existing position unchanged.
    Hold,
    /// Do nothing, for a recorded reason.
    NoTrade {
        /// Why no trade.
        reason: NoTradeReason,
    },
}

impl DecisionOutcome {
    /// The headline the UI shows (spec §6.3 labels).
    #[must_use]
    pub const fn headline(&self) -> &'static str {
        match self {
            Self::Enter { action } => action.action().label(),
            Self::Exit { action, .. } => action.action().label(),
            Self::Hold => "HOLD",
            Self::NoTrade { .. } => "NO TRADE",
        }
    }
}

/// Why a position is closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitReason {
    /// The stop loss was hit.
    StopHit,
    /// The target was hit.
    TargetHit,
    /// The maximum holding period elapsed.
    TimeExit,
    /// An invalidation rule fired.
    Invalidated,
    /// The owner closed it manually.
    Manual,
    /// A flatten request closed it.
    Flatten,
    /// A futures position was rolled to the next contract.
    Roll,
    /// The strategy version was demoted.
    Demotion,
}

/// An input the decision depends on (INV-06).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputKind {
    /// Quotes or last prices.
    Prices,
    /// Daily bars.
    Bars,
    /// Account equity.
    Equity,
    /// Positions and holdings.
    Positions,
    /// FX rates.
    FxRates,
    /// Trading calendars.
    Calendar,
    /// Configuration.
    Configuration,
    /// Kill-switch state.
    KillSwitchState,
    /// Instrument specs.
    InstrumentSpec,
}

/// A risk limit that blocked an entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "breach", rename_all = "snake_case")]
pub enum RiskLimitBreach {
    /// Total open risk would exceed the account cap.
    TotalOpenRisk {
        /// Cap as a fraction of equity.
        limit: Ratio,
        /// Open risk including this entry, as a fraction of equity.
        would_be: Ratio,
    },
    /// Open risk in a correlated bucket would exceed its cap.
    CorrelatedBucket {
        /// The bucket.
        bucket: CorrelationBucket,
        /// Cap as a fraction of equity.
        limit: Ratio,
        /// Bucket risk including this entry, as a fraction of equity.
        would_be: Ratio,
    },
    /// Open risk for the strategy version would exceed its cap.
    StrategyVersion {
        /// Cap as a fraction of equity.
        limit: Ratio,
        /// Strategy risk including this entry, as a fraction of equity.
        would_be: Ratio,
    },
    /// The daily loss limit is reached.
    DailyLoss {
        /// Limit as a fraction of start-of-day equity.
        limit: Ratio,
        /// Current loss as a fraction of start-of-day equity.
        current: Ratio,
    },
    /// The weekly loss limit is reached.
    WeeklyLoss {
        /// Limit as a fraction of start-of-week equity.
        limit: Ratio,
        /// Current loss as a fraction of start-of-week equity.
        current: Ratio,
    },
    /// Drawdown from the equity high-water mark reached its limit.
    Drawdown {
        /// Limit as a fraction of the high-water mark.
        limit: Ratio,
        /// Current drawdown.
        current: Ratio,
    },
    /// The consecutive-loss cool-off is active.
    ConsecutiveLosses {
        /// Losing trades in a row.
        losses: u32,
        /// Cool-off threshold.
        limit: u32,
    },
    /// The strategy version's stage may not trade on this account.
    StageNotEligible {
        /// The version's stage.
        stage: StrategyStage,
    },
}

/// Why the decision is NO TRADE.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "code", content = "detail", rename_all = "snake_case")]
pub enum NoTradeReason {
    /// The strategy is not active in the current regime.
    StrategyInactiveInRegime {
        /// The classified regime.
        regime: String,
    },
    /// The trade plan is invalid.
    InvalidTradePlan {
        /// What is wrong with it.
        defect: PlanDefect,
    },
    /// Too few comparable out-of-sample setups back the probabilities.
    InsufficientEvidence {
        /// Comparable setups available.
        n: u32,
        /// Minimum required.
        min: u32,
    },
    /// Expected value after costs is below the minimum.
    InsufficientEdge {
        /// Expected value in R.
        ev_r: Decimal,
        /// Minimum required, in R.
        min: Decimal,
    },
    /// Net reward-to-risk is below the strategy's floor.
    RiskRewardBelowFloor {
        /// Net RR.
        rr: Decimal,
        /// The floor.
        floor: Decimal,
    },
    /// Signals disagree.
    ConflictingSignals,
    /// A newer proposal replaced this one.
    Superseded,
    /// A position in this instrument is already open.
    AlreadyInPosition,
    /// The instrument or the strategy version does not permit this short.
    ShortNotPermitted,
    /// An input is older than its freshness limit.
    StaleData {
        /// Which input.
        input: InputKind,
    },
    /// An input is missing or inconsistent.
    MissingOrInconsistentData {
        /// Which input.
        input: InputKind,
    },
    /// The market is closed or the instrument is halted by the venue.
    MarketClosedOrHalted,
    /// The price has already moved past the planned entry; the setup is kept on watch.
    PriceMovedPastEntry {
        /// The planned entry.
        entry: Price,
        /// The last observed price.
        last: Price,
    },
    /// A risk limit blocked the entry.
    RiskLimit(RiskLimitBreach),
    /// A halt (kill switch) blocked the entry.
    KillSwitch(HaltBlock),
    /// The sized position is below the minimum tradable quantity.
    PositionTooSmall {
        /// Quantity after rounding and caps.
        quantity: Quantity,
        /// Minimum tradable quantity.
        min: Quantity,
    },
    /// Costs at the final quantity make the trade uneconomic.
    UneconomicAfterCosts,
    /// A scheduled event makes the trade too risky.
    EventRisk,
    /// Market conditions are abnormal.
    AbnormalMarket,
    /// An AI review vetoed the trade. Only valid where the strategy's AI policy allows it.
    AiVeto {
        /// The review that vetoed it.
        review: AiReviewId,
    },
}

impl NoTradeReason {
    /// A stable machine code, used in the journal and by the UI.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::StrategyInactiveInRegime { .. } => "strategy_inactive_in_regime",
            Self::InvalidTradePlan { .. } => "invalid_trade_plan",
            Self::InsufficientEvidence { .. } => "insufficient_evidence",
            Self::InsufficientEdge { .. } => "insufficient_edge",
            Self::RiskRewardBelowFloor { .. } => "risk_reward_below_floor",
            Self::ConflictingSignals => "conflicting_signals",
            Self::Superseded => "superseded",
            Self::AlreadyInPosition => "already_in_position",
            Self::ShortNotPermitted => "short_not_permitted",
            Self::StaleData { .. } => "stale_data",
            Self::MissingOrInconsistentData { .. } => "missing_or_inconsistent_data",
            Self::MarketClosedOrHalted => "market_closed_or_halted",
            Self::PriceMovedPastEntry { .. } => "price_moved_past_entry",
            Self::RiskLimit(_) => "risk_limit",
            Self::KillSwitch(_) => "kill_switch",
            Self::PositionTooSmall { .. } => "position_too_small",
            Self::UneconomicAfterCosts => "uneconomic_after_costs",
            Self::EventRisk => "event_risk",
            Self::AbnormalMarket => "abnormal_market",
            Self::AiVeto { .. } => "ai_veto",
        }
    }
}

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;

    use super::*;

    #[test]
    fn no_trade_reasons_serialize_with_their_code() {
        let reason = NoTradeReason::InsufficientEvidence { n: 12, min: 30 };
        let json = serde_json::to_value(&reason).unwrap();
        assert_eq!(json["code"], reason.code());
        assert_eq!(json["detail"]["n"], 12);
        let back: NoTradeReason = serde_json::from_value(json).unwrap();
        assert_eq!(back, reason);
    }

    #[test]
    fn nested_risk_limit_reasons_round_trip() {
        let reason = NoTradeReason::RiskLimit(RiskLimitBreach::DailyLoss {
            limit: Ratio::new(dec!(0.02)).unwrap(),
            current: Ratio::new(dec!(0.021)).unwrap(),
        });
        let json = serde_json::to_string(&reason).unwrap();
        assert_eq!(
            serde_json::from_str::<NoTradeReason>(&json).unwrap(),
            reason
        );
    }
}
