//! Decision Journal entries (spec §5.2, INV-05, INV-16).
//!
//! Every decision, NO TRADE included, and every order and position event is an
//! append-only entry. Entries are written before any broker call they describe.

use chrono::{DateTime, NaiveDate, Utc};
use qd_domain::halt::Halt;
use qd_domain::ids::{AccountId, DecisionId, InstrumentId, OrderIntentId, PositionId};
use qd_domain::lifecycle::order::{OrderEvent, OrderIntentState};
use qd_domain::lifecycle::position::{PositionEvent, PositionState};
use qd_domain::num::{Price, Quantity};
use qd_domain::outcome::DecisionOutcome;
use qd_domain::proposal::{StrategyRef, TradeProposal};
use qd_risk::gate::ApprovedEntry;
use qd_strategy::regime::Regime;
use serde::Serialize;

use crate::orders::OrderIntent;

/// One journaled decision with everything needed to explain and replay it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DecisionRecord {
    /// Decision id.
    pub id: DecisionId,
    /// Decision time.
    pub at: DateTime<Utc>,
    /// Last completed trading date the decision used.
    pub as_of_date: NaiveDate,
    /// Account.
    pub account: AccountId,
    /// Instrument.
    pub instrument: InstrumentId,
    /// Strategy version.
    pub strategy: StrategyRef,
    /// Regime at the decision.
    pub regime: Regime,
    /// Feature-set version.
    pub feature_set_version: String,
    /// The post-risk outcome (INV-17).
    pub outcome: DecisionOutcome,
    /// The proposal, when one was built.
    pub proposal: Option<TradeProposal>,
    /// The Risk Gate approval, when there was one.
    pub approval: Option<ApprovedEntry>,
}

/// One journal entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JournalEntry {
    /// A decision.
    Decision(Box<DecisionRecord>),
    /// An order intent, before it is sent.
    OrderIntent {
        /// The intent.
        intent: Box<OrderIntent>,
    },
    /// An order state transition.
    OrderEvent {
        /// Intent.
        intent: OrderIntentId,
        /// Event.
        event: OrderEvent,
        /// State after the event.
        state: OrderIntentState,
        /// When.
        at: DateTime<Utc>,
    },
    /// A fill.
    Fill {
        /// Intent.
        intent: OrderIntentId,
        /// Quantity.
        quantity: Quantity,
        /// Price.
        price: Price,
        /// When.
        at: DateTime<Utc>,
    },
    /// A position state transition.
    PositionEvent {
        /// Position.
        position: PositionId,
        /// Event.
        event: PositionEvent,
        /// State after the event.
        state: PositionState,
        /// When.
        at: DateTime<Utc>,
    },
    /// The full state of a position after a change (restore source).
    PositionSnapshot {
        /// The position.
        position: Box<crate::positions::Position>,
        /// When.
        at: DateTime<Utc>,
    },
    /// A trading day finished for an account: its book and closed trades.
    DayClosed(Box<crate::session::DayRecord>),
    /// A halt was recorded or cleared.
    Halt {
        /// The halt.
        halt: Box<Halt>,
    },
}
