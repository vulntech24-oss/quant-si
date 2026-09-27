//! Rebuilding trading state from the journal after a restart (ADR 0009).
//!
//! The journal is the source of truth (INV-05, INV-16). Order intents, order
//! events and fills rebuild the Order Gateway; position snapshots rebuild the
//! Position Manager; the last `day_closed` entry restores the account book.
//!
//! Rules that keep a restore on the safe side:
//!
//! - An intent journaled without an acceptance was never sent: it is
//!   restored as gateway-rejected.
//! - An intent's fill state follows its journaled fills.
//! - An intent accepted but without a journaled broker result may or may not
//!   have reached the broker: it is restored as `Unknown`, and only
//!   reconciliation may resolve it.
//! - [`RestoredState::check`] refuses a state where the order ledger and the
//!   positions disagree. Callers must not trade on a state that fails it.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::NaiveDate;
use qd_domain::action::{RiskEffect, Side, Transfer};
use qd_domain::ids::{AccountId, InstrumentId, OrderIntentId, PositionId};
use qd_domain::lifecycle::order::OrderIntentState;
use qd_domain::lifecycle::position::PositionState;
use qd_domain::num::Quantity;
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;

use crate::orders::{BrokerOrderRequest, OrderIntent};
use crate::ports::BrokerPosition;
use crate::positions::Position;
use crate::session::DayRecord;

/// Journal kinds that carry trading state.
pub const STATE_KINDS: [&str; 5] = [
    "order_intent",
    "order_event",
    "fill",
    "position_snapshot",
    "day_closed",
];

/// Why a restore failed. Trading must not resume on a failed restore.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RestoreError {
    /// An entry could not be read.
    #[error("journal entry {index} ({kind}) is malformed: {detail}")]
    Malformed {
        /// Position of the entry in the replay.
        index: usize,
        /// Entry kind.
        kind: String,
        /// Parser message.
        detail: String,
    },
    /// The rebuilt state contradicts itself.
    #[error("restored state is inconsistent: {0}")]
    Inconsistent(String),
}

/// One restored intent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestoredIntent {
    /// The intent as journaled.
    pub intent: OrderIntent,
    /// State after its last journaled event.
    pub state: OrderIntentState,
    /// Quantity filled per journaled fills.
    pub filled: Quantity,
    /// The broker's id for the order, once acknowledged.
    pub broker_order_id: Option<String>,
}

/// Trading state rebuilt from the journal for one account.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RestoredState {
    /// Intents, in journal order.
    pub intents: Vec<RestoredIntent>,
    /// Latest snapshot of every position, by id.
    pub positions: Vec<Position>,
    /// The last trading day processed.
    pub last_day: Option<DayRecord>,
    /// Positions whose closed trade was already booked.
    pub recorded: BTreeSet<PositionId>,
}

#[derive(Deserialize)]
struct EventEntry {
    intent: OrderIntentId,
    state: OrderIntentState,
}

#[derive(Deserialize)]
struct FillEntry {
    intent: OrderIntentId,
    quantity: Quantity,
}

#[derive(Deserialize)]
struct SnapshotEntry {
    position: Position,
}

impl RestoredState {
    /// Replays journal entries (their JSON, oldest first) for one account.
    pub fn from_entries<'a, I>(account: AccountId, entries: I) -> Result<Self, RestoreError>
    where
        I: IntoIterator<Item = &'a Value>,
    {
        let mut intents: Vec<RestoredIntent> = Vec::new();
        let mut index_of: HashMap<OrderIntentId, usize> = HashMap::new();
        let mut journaled_events: BTreeSet<OrderIntentId> = BTreeSet::new();
        let mut positions: BTreeMap<PositionId, Position> = BTreeMap::new();
        let mut last_day: Option<DayRecord> = None;
        let mut recorded = BTreeSet::new();

        for (index, value) in entries.into_iter().enumerate() {
            let kind = value.get("kind").and_then(Value::as_str).unwrap_or("");
            let malformed = |detail: String| RestoreError::Malformed {
                index,
                kind: kind.to_owned(),
                detail,
            };
            match kind {
                "order_intent" => {
                    let raw = value
                        .get("intent")
                        .ok_or_else(|| malformed("missing intent".to_owned()))?;
                    let intent =
                        OrderIntent::from_journal(raw).map_err(|e| malformed(e.to_string()))?;
                    if intent.account() != account || index_of.contains_key(&intent.id()) {
                        continue;
                    }
                    index_of.insert(intent.id(), intents.len());
                    intents.push(RestoredIntent {
                        intent,
                        state: OrderIntentState::Created,
                        filled: Quantity::ZERO,
                        broker_order_id: None,
                    });
                }
                "order_event" => {
                    let e: EventEntry = serde_json::from_value(value.clone())
                        .map_err(|e| malformed(e.to_string()))?;
                    if let Some(&i) = index_of.get(&e.intent) {
                        intents[i].state = e.state;
                        if let Some(id) = value
                            .pointer("/event/broker_order_id")
                            .and_then(Value::as_str)
                        {
                            intents[i].broker_order_id = Some(id.to_owned());
                        }
                        journaled_events.insert(e.intent);
                    }
                }
                "fill" => {
                    let f: FillEntry = serde_json::from_value(value.clone())
                        .map_err(|e| malformed(e.to_string()))?;
                    if let Some(&i) = index_of.get(&f.intent) {
                        let r = &mut intents[i];
                        let filled = r.filled.value() + f.quantity.value();
                        if filled > r.intent.quantity().value() {
                            return Err(RestoreError::Inconsistent(format!(
                                "intent {} filled beyond its quantity",
                                f.intent
                            )));
                        }
                        r.filled = Quantity::new(filled).map_err(|e| malformed(e.to_string()))?;
                    }
                }
                "position_snapshot" => {
                    let s: SnapshotEntry = serde_json::from_value(value.clone())
                        .map_err(|e| malformed(e.to_string()))?;
                    if s.position.account == account {
                        positions.insert(s.position.id, s.position);
                    }
                }
                "day_closed" => {
                    let d: DayRecord = serde_json::from_value(value.clone())
                        .map_err(|e| malformed(e.to_string()))?;
                    if d.account == account {
                        recorded.extend(d.trades.iter().map(|t| t.position));
                        last_day = Some(d);
                    }
                }
                _ => {}
            }
        }

        for r in &mut intents {
            // Fills are journaled as `fill` entries, not as order events, so
            // the fill state comes from the filled quantity.
            let complete = r.filled == r.intent.quantity();
            let partial = !r.filled.is_zero() && !complete;
            r.state = match r.state {
                OrderIntentState::Submitted | OrderIntentState::PartiallyFilled if complete => {
                    OrderIntentState::Filled
                }
                OrderIntentState::Submitted if partial => OrderIntentState::PartiallyFilled,
                // Journaled, never accepted: never sent.
                OrderIntentState::Created if !journaled_events.contains(&r.intent.id()) => {
                    OrderIntentState::GatewayRejected
                }
                // Accepted, but the broker's answer was not journaled.
                OrderIntentState::Created | OrderIntentState::PendingSubmit => {
                    OrderIntentState::Unknown
                }
                other => other,
            };
        }

        Ok(Self {
            intents,
            positions: positions.into_values().collect(),
            last_day,
            recorded,
        })
    }

    /// Positions not yet closed.
    #[must_use]
    pub fn active_positions(&self) -> Vec<Position> {
        self.positions
            .iter()
            .filter(|p| p.state != PositionState::Closed)
            .cloned()
            .collect()
    }

    /// Checks that the order ledger and the positions agree: per instrument
    /// and side, and per position.
    pub fn check(&self) -> Result<(), RestoreError> {
        let mut ledger: HashMap<(InstrumentId, Side), Decimal> = HashMap::new();
        let mut per_position: BTreeMap<PositionId, Decimal> = BTreeMap::new();
        for r in &self.intents {
            let signed = match r.intent.risk_effect() {
                RiskEffect::Increasing => r.filled.value(),
                RiskEffect::Reducing => -r.filled.value(),
            };
            *ledger
                .entry((r.intent.instrument(), r.intent.action().side()))
                .or_default() += signed;
            if let Some(p) = r.intent.position() {
                *per_position.entry(p).or_default() += signed;
            }
        }
        let mut book: HashMap<(InstrumentId, Side), Decimal> = HashMap::new();
        for p in &self.positions {
            let from_orders = per_position.get(&p.id).copied().unwrap_or_default();
            if from_orders != p.quantity.value() {
                return Err(RestoreError::Inconsistent(format!(
                    "position {} holds {} but its fills net to {from_orders}",
                    p.id,
                    p.quantity.value()
                )));
            }
            *book.entry((p.instrument, p.side)).or_default() += p.quantity.value();
        }
        for (key, quantity) in &ledger {
            if *quantity < Decimal::ZERO {
                return Err(RestoreError::Inconsistent(format!(
                    "exits exceed entries on instrument {}",
                    key.0
                )));
            }
            if book.get(key).copied().unwrap_or_default() != *quantity {
                return Err(RestoreError::Inconsistent(format!(
                    "order ledger and positions disagree on instrument {}",
                    key.0
                )));
            }
        }
        if let Some(p) = per_position
            .keys()
            .find(|id| !self.positions.iter().any(|p| p.id == **id))
        {
            return Err(RestoreError::Inconsistent(format!(
                "orders reference position {p}, which has no snapshot"
            )));
        }
        Ok(())
    }

    /// Orders working at a simulated venue, with the date each was placed,
    /// for restoring the paper venue. Each was sent by the Order Gateway
    /// before the restart; restoring them sends nothing.
    #[must_use]
    pub fn working_orders(
        &self,
        symbols: &HashMap<InstrumentId, String>,
    ) -> Vec<(BrokerOrderRequest, NaiveDate)> {
        self.intents
            .iter()
            .filter(|r| {
                matches!(
                    r.state,
                    OrderIntentState::Submitted | OrderIntentState::PartiallyFilled
                )
            })
            .map(|r| {
                let symbol = symbols
                    .get(&r.intent.instrument())
                    .map_or("", String::as_str);
                (
                    BrokerOrderRequest::from_intent(&r.intent, symbol),
                    r.intent.created_at().date_naive(),
                )
            })
            .collect()
    }

    /// Net positions implied by the journaled fills (a simulated venue's book).
    #[must_use]
    pub fn net_positions(&self) -> Vec<BrokerPosition> {
        let mut net: BTreeMap<InstrumentId, Decimal> = BTreeMap::new();
        for r in &self.intents {
            let signed = match r.intent.action().transfer() {
                Transfer::Purchase => r.filled.value(),
                Transfer::Disposal => -r.filled.value(),
            };
            *net.entry(r.intent.instrument()).or_default() += signed;
        }
        net.into_iter()
            .filter(|(_, q)| !q.is_zero())
            .map(|(instrument, net_quantity)| BrokerPosition {
                instrument,
                net_quantity,
            })
            .collect()
    }
}
