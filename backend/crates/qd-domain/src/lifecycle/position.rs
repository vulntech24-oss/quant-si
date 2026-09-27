//! Position lifecycle (spec §6.6).
//!
//! Opening → Open → Protected ⇄ Unprotected → Exiting → Closed.
//! Additions, recorded in ADR 0003: an entry that never fills goes
//! Opening → Closed, and a failed exit goes Exiting → Unprotected so that the
//! position is counted at a gap shock and raises an alert.

use serde::{Deserialize, Serialize};

use super::TransitionError;
use crate::outcome::ExitReason;

/// State of one position.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PositionState {
    /// Entry order working, nothing filled yet.
    Opening,
    /// Filled; protection not yet confirmed.
    Open,
    /// Filled and protected by a working broker-side stop.
    Protected,
    /// Filled without valid protection. Counted at a gap shock; raises an alert.
    Unprotected,
    /// An exit is in progress.
    Exiting,
    /// Flat. Terminal.
    Closed,
}

/// A position event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum PositionEvent {
    /// The entry order (partly) filled.
    EntryFilled,
    /// The entry order ended with no fill.
    EntryAbandoned,
    /// Broker-side protection is confirmed working.
    ProtectionConfirmed,
    /// Protection could not be placed, or is no longer valid.
    ProtectionLost,
    /// An exit started.
    ExitStarted {
        /// Why.
        reason: ExitReason,
    },
    /// The exit did not complete.
    ExitFailed,
    /// The position is flat.
    ExitCompleted,
}

impl PositionEvent {
    /// Event name for errors and logs.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::EntryFilled => "entry_filled",
            Self::EntryAbandoned => "entry_abandoned",
            Self::ProtectionConfirmed => "protection_confirmed",
            Self::ProtectionLost => "protection_lost",
            Self::ExitStarted { .. } => "exit_started",
            Self::ExitFailed => "exit_failed",
            Self::ExitCompleted => "exit_completed",
        }
    }
}

impl PositionState {
    /// Applies an event, returning the next state or an error for an illegal transition.
    pub fn apply(self, event: &PositionEvent) -> Result<Self, TransitionError> {
        use PositionEvent as E;
        use PositionState as S;
        let next = match (self, event) {
            (S::Opening, E::EntryFilled) => Some(S::Open),
            (S::Opening, E::EntryAbandoned) => Some(S::Closed),
            (S::Open | S::Unprotected, E::ProtectionConfirmed) => Some(S::Protected),
            (S::Open | S::Protected, E::ProtectionLost) => Some(S::Unprotected),
            (S::Open | S::Protected | S::Unprotected, E::ExitStarted { .. }) => Some(S::Exiting),
            (S::Exiting, E::ExitFailed) => Some(S::Unprotected),
            (S::Exiting, E::ExitCompleted) => Some(S::Closed),
            _ => None,
        };
        next.ok_or_else(|| TransitionError::new("position", self, event.name()))
    }
}
