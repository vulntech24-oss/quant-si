//! Trade actions and their user-facing labels (spec §6.3, INV-12).
//!
//! The domain has no ambiguous order-direction words, only the four explicit
//! actions. Only broker adapters translate an action into a broker transaction
//! type.

use serde::{Deserialize, Serialize};

/// Position direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    /// Profits when the price rises.
    Long,
    /// Profits when the price falls.
    Short,
}

impl Side {
    /// The action that opens a position on this side.
    #[must_use]
    pub const fn entry(self) -> EntryAction {
        match self {
            Self::Long => EntryAction::OpenLong,
            Self::Short => EntryAction::OpenShort,
        }
    }

    /// The action that closes a position on this side.
    #[must_use]
    pub const fn exit(self) -> ExitAction {
        match self {
            Self::Long => ExitAction::CloseLong,
            Self::Short => ExitAction::CloseShort,
        }
    }
}

/// Whether an order can increase or only reduce risk (INV-02).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskEffect {
    /// Opens or adds to a position. Blocked by any applicable halt.
    Increasing,
    /// Exits, protective stops and targets, flatten. Never blocked by a halt.
    Reducing,
}

/// The four explicit trade actions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TradeAction {
    /// Open (or add to) a long position.
    OpenLong,
    /// Close (part of) a long position.
    CloseLong,
    /// Open (or add to) a short position.
    OpenShort,
    /// Close (part of) a short position.
    CloseShort,
}

impl TradeAction {
    /// All actions, in a stable order.
    pub const ALL: [Self; 4] = [
        Self::OpenLong,
        Self::CloseLong,
        Self::OpenShort,
        Self::CloseShort,
    ];

    /// The side of the position this action opens or closes.
    #[must_use]
    pub const fn side(self) -> Side {
        match self {
            Self::OpenLong | Self::CloseLong => Side::Long,
            Self::OpenShort | Self::CloseShort => Side::Short,
        }
    }

    /// Whether the action increases or reduces risk.
    #[must_use]
    pub const fn risk_effect(self) -> RiskEffect {
        match self {
            Self::OpenLong | Self::OpenShort => RiskEffect::Increasing,
            Self::CloseLong | Self::CloseShort => RiskEffect::Reducing,
        }
    }

    /// The exact label the UI shows for this action (spec §6.3 table).
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::OpenLong => "BUY (open long)",
            Self::CloseLong => "SELL (close long)",
            Self::OpenShort => "SELL SHORT (open short)",
            Self::CloseShort => "BUY TO COVER (close short)",
        }
    }

    /// Whether the action acquires or disposes of units. Statutory charges
    /// (transaction taxes, stamp duty) depend on this, not on the side.
    #[must_use]
    pub const fn transfer(self) -> Transfer {
        match self {
            Self::OpenLong | Self::CloseShort => Transfer::Purchase,
            Self::CloseLong | Self::OpenShort => Transfer::Disposal,
        }
    }
}

/// Direction of the transfer of units in one order, used only for charges.
///
/// OpenLong and CloseShort acquire units; CloseLong and OpenShort dispose of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transfer {
    /// Units are acquired.
    Purchase,
    /// Units are disposed of.
    Disposal,
}

/// An action that opens a position. `DecisionOutcome::Enter` can only hold these.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryAction {
    /// Open a long position.
    OpenLong,
    /// Open a short position.
    OpenShort,
}

impl EntryAction {
    /// The side this entry opens.
    #[must_use]
    pub const fn side(self) -> Side {
        match self {
            Self::OpenLong => Side::Long,
            Self::OpenShort => Side::Short,
        }
    }

    /// The general action.
    #[must_use]
    pub const fn action(self) -> TradeAction {
        match self {
            Self::OpenLong => TradeAction::OpenLong,
            Self::OpenShort => TradeAction::OpenShort,
        }
    }
}

impl From<EntryAction> for TradeAction {
    fn from(action: EntryAction) -> Self {
        action.action()
    }
}

/// An action that closes a position. `DecisionOutcome::Exit` can only hold these.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitAction {
    /// Close a long position.
    CloseLong,
    /// Close a short position.
    CloseShort,
}

impl ExitAction {
    /// The side this exit closes.
    #[must_use]
    pub const fn side(self) -> Side {
        match self {
            Self::CloseLong => Side::Long,
            Self::CloseShort => Side::Short,
        }
    }

    /// The general action.
    #[must_use]
    pub const fn action(self) -> TradeAction {
        match self {
            Self::CloseLong => TradeAction::CloseLong,
            Self::CloseShort => TradeAction::CloseShort,
        }
    }
}

impl From<ExitAction> for TradeAction {
    fn from(action: ExitAction) -> Self {
        action.action()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_increase_and_exits_reduce_risk() {
        for action in TradeAction::ALL {
            let expected = match action {
                TradeAction::OpenLong | TradeAction::OpenShort => RiskEffect::Increasing,
                TradeAction::CloseLong | TradeAction::CloseShort => RiskEffect::Reducing,
            };
            assert_eq!(action.risk_effect(), expected);
        }
    }

    #[test]
    fn entry_and_exit_actions_map_to_the_matching_side() {
        for side in [Side::Long, Side::Short] {
            assert_eq!(side.entry().side(), side);
            assert_eq!(side.exit().side(), side);
            assert_eq!(TradeAction::from(side.entry()).side(), side);
            assert_eq!(TradeAction::from(side.exit()).side(), side);
        }
    }
}
