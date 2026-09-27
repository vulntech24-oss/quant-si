//! Order-intent lifecycle (spec §6.6).
//!
//! Created → GatewayRejected, or Created → PendingSubmit → Submitted →
//! PartiallyFilled → Filled | Cancelled | BrokerRejected | Expired.
//! A timeout or transport error moves PendingSubmit, Submitted or
//! PartiallyFilled to Unknown, which only reconciliation resolves.
//! Terminal states accept no events.

use serde::{Deserialize, Serialize};

use super::TransitionError;

/// State of one order intent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderIntentState {
    /// Recorded, not yet validated by the Gateway.
    Created,
    /// The Gateway refused it. Terminal.
    GatewayRejected,
    /// Accepted by the Gateway and journaled; the broker call is about to be made.
    PendingSubmit,
    /// The broker acknowledged it.
    Submitted,
    /// Partly filled.
    PartiallyFilled,
    /// Completely filled. Terminal.
    Filled,
    /// Cancelled. Terminal.
    Cancelled,
    /// The broker refused it. Terminal.
    BrokerRejected,
    /// Expired at the end of its validity. Terminal.
    Expired,
    /// Outcome unknown after a timeout or transport error. Resolved only by reconciliation.
    Unknown,
}

impl OrderIntentState {
    /// Whether the state is terminal (immutable).
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::GatewayRejected
                | Self::Filled
                | Self::Cancelled
                | Self::BrokerRejected
                | Self::Expired
        )
    }
}

/// A state observed at the broker during reconciliation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconciledState {
    /// Working at the broker.
    Submitted,
    /// Partly filled at the broker.
    PartiallyFilled,
    /// Filled at the broker.
    Filled,
    /// Cancelled at the broker.
    Cancelled,
    /// Rejected by the broker, or never reached it.
    BrokerRejected,
    /// Expired at the broker.
    Expired,
}

impl From<ReconciledState> for OrderIntentState {
    fn from(state: ReconciledState) -> Self {
        match state {
            ReconciledState::Submitted => Self::Submitted,
            ReconciledState::PartiallyFilled => Self::PartiallyFilled,
            ReconciledState::Filled => Self::Filled,
            ReconciledState::Cancelled => Self::Cancelled,
            ReconciledState::BrokerRejected => Self::BrokerRejected,
            ReconciledState::Expired => Self::Expired,
        }
    }
}

/// An order-intent event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum OrderEvent {
    /// The Gateway accepted and journaled the intent.
    GatewayAccepted,
    /// The Gateway refused the intent.
    GatewayRejected {
        /// Why.
        reason: String,
    },
    /// The broker acknowledged the order.
    BrokerAcknowledged {
        /// The broker's order id.
        broker_order_id: String,
    },
    /// A partial fill.
    PartiallyFilled,
    /// The order is completely filled.
    Filled,
    /// The order was cancelled.
    Cancelled,
    /// The broker refused the order.
    BrokerRejected {
        /// Why.
        reason: String,
    },
    /// The order expired.
    Expired,
    /// A timeout or transport error left the outcome unknown.
    TransportFailure {
        /// What failed.
        detail: String,
    },
    /// Reconciliation established the broker-side state.
    Reconciled {
        /// The observed state.
        observed: ReconciledState,
    },
}

impl OrderEvent {
    /// Event name for errors and logs.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::GatewayAccepted => "gateway_accepted",
            Self::GatewayRejected { .. } => "gateway_rejected",
            Self::BrokerAcknowledged { .. } => "broker_acknowledged",
            Self::PartiallyFilled => "partially_filled",
            Self::Filled => "filled",
            Self::Cancelled => "cancelled",
            Self::BrokerRejected { .. } => "broker_rejected",
            Self::Expired => "expired",
            Self::TransportFailure { .. } => "transport_failure",
            Self::Reconciled { .. } => "reconciled",
        }
    }
}

impl OrderIntentState {
    /// Applies an event, returning the next state or an error for an illegal transition.
    pub fn apply(self, event: &OrderEvent) -> Result<Self, TransitionError> {
        use OrderEvent as E;
        use OrderIntentState as S;
        let next = match (self, event) {
            (S::Created, E::GatewayAccepted) => Some(S::PendingSubmit),
            (S::Created, E::GatewayRejected { .. }) => Some(S::GatewayRejected),
            (S::PendingSubmit, E::BrokerAcknowledged { .. }) => Some(S::Submitted),
            (S::PendingSubmit, E::BrokerRejected { .. }) => Some(S::BrokerRejected),
            (S::Submitted | S::PartiallyFilled, E::PartiallyFilled) => Some(S::PartiallyFilled),
            (S::Submitted | S::PartiallyFilled, E::Filled) => Some(S::Filled),
            (S::Submitted | S::PartiallyFilled, E::Cancelled) => Some(S::Cancelled),
            (S::Submitted | S::PartiallyFilled, E::Expired) => Some(S::Expired),
            (S::Submitted, E::BrokerRejected { .. }) => Some(S::BrokerRejected),
            (S::PendingSubmit | S::Submitted | S::PartiallyFilled, E::TransportFailure { .. }) => {
                Some(S::Unknown)
            }
            (S::Unknown, E::Reconciled { observed }) => Some(S::from(*observed)),
            _ => None,
        };
        next.ok_or_else(|| TransitionError::new("order intent", self, event.name()))
    }
}
