//! State machines (spec §6.6).
//!
//! Each machine is an explicit enum with a transition function that returns
//! `Result`. Illegal transitions are errors. Events are serializable so every
//! transition can be persisted as an append-only event (INV-16).

use thiserror::Error;

pub mod order;
pub mod position;
pub mod strategy;

/// An illegal state transition.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("illegal {machine} transition: {event} from {from}")]
pub struct TransitionError {
    /// Which state machine.
    pub machine: &'static str,
    /// The state the event was applied to.
    pub from: String,
    /// The event name.
    pub event: &'static str,
}

impl TransitionError {
    pub(crate) fn new(
        machine: &'static str,
        from: impl std::fmt::Debug,
        event: &'static str,
    ) -> Self {
        Self {
            machine,
            from: format!("{from:?}"),
            event,
        }
    }
}
