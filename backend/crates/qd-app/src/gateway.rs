//! The Order Gateway: the only order path (INV-01), with the kill switch inside
//! it (INV-02), journal-before-send (INV-05), idempotency and the live gate (INV-14).
//!
//! For every intent, in order:
//!
//! 1. Idempotency: a known intent id returns its current state; the broker is
//!    never called twice for one intent.
//! 2. Validation: account, instrument, quantity step and minimum, order terms
//!    against the instrument's capabilities.
//! 3. Risk-increasing orders: an entry authorization covering the quantity,
//!    overnight-short permission, no journal-failure latch, and no active
//!    halt (an unreadable halt store counts as halted).
//!    Risk-reducing orders: the quantity plus other working exits must not
//!    exceed the open quantity (orders in one OCO group count once).
//! 4. Live accounts: every INV-14 condition.
//! 5. The intent and its acceptance are journaled. If that fails nothing is
//!    sent and new entries are halted.
//! 6. The broker call. A transport error leaves the intent `Unknown`, to be
//!    resolved only by reconciliation.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use chrono::{DateTime, Utc};
use qd_domain::action::{RiskEffect, Side, TradeAction};
use qd_domain::halt::{HaltBlock, HaltState, OrderContext, check_order};
use qd_domain::ids::{AccountId, InstrumentId, OrderIntentId, PositionId};
use qd_domain::instrument::{InstrumentSpec, OrderType};
use qd_domain::lifecycle::order::{OrderEvent, OrderIntentState, ReconciledState};
use qd_domain::lifecycle::strategy::StrategyStage;
use qd_domain::num::{Price, Quantity};
use qd_domain::order_rules::{ExitQuantityError, check_exit_quantity};
use qd_domain::proposal::AccountMode;
use serde::Serialize;
use thiserror::Error;

use crate::journal::JournalEntry;
use crate::live::{LiveBlock, LivePolicy, check_live_order};
use crate::orders::{BrokerOrderRequest, OrderIntent};
use crate::ports::{BrokerError, BrokerFill, BrokerOrderExecutor, Clock, HaltStore, Journal};

/// The account a gateway serves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GatewayAccount {
    /// Account id.
    pub id: AccountId,
    /// Backtest, paper or live.
    pub mode: AccountMode,
    /// Whether the owner armed live trading (step-up authenticated, INV-14).
    pub live_armed: bool,
}

/// Why the gateway refused an intent or an event.
#[derive(Debug, Clone, PartialEq, Eq, Error, Serialize)]
#[serde(tag = "rejection", content = "detail", rename_all = "snake_case")]
pub enum GatewayRejection {
    /// The intent is for another account.
    #[error("intent is for another account")]
    WrongAccount,
    /// The instrument is not known to the gateway.
    #[error("unknown instrument")]
    UnknownInstrument,
    /// The quantity is not a valid order size for the instrument.
    #[error("quantity is not a valid order size")]
    InvalidQuantity,
    /// Order type, prices or product do not fit the instrument.
    #[error("invalid order terms: {0}")]
    InvalidTerms(String),
    /// A risk-increasing intent without a covering entry authorization.
    #[error("entry exceeds or lacks its Risk Gate authorization")]
    NotAuthorized,
    /// The instrument does not allow this short overnight.
    #[error("short not permitted on this instrument")]
    ShortNotPermitted,
    /// A halt blocks risk-increasing orders.
    #[error("halted: {0:?}")]
    Halted(HaltBlock),
    /// The exit would exceed the open quantity.
    #[error("exit exceeds open quantity: {0}")]
    ExitExceedsOpen(ExitQuantityError),
    /// Live-trading conditions are not met.
    #[error("live order not permitted: {0}")]
    LiveNotPermitted(LiveBlock),
    /// The journal could not be written; nothing was sent (INV-05).
    #[error("journal unavailable; nothing was sent")]
    JournalUnavailable,
    /// The broker refused the order.
    #[error("broker rejected: {0}")]
    BrokerRejected(String),
    /// The intent id is unknown.
    #[error("unknown intent")]
    UnknownIntent,
    /// The event does not fit the intent's state or quantity.
    #[error("invalid order event: {0}")]
    InvalidEvent(String),
}

/// The gateway's answer to a submission.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct GatewayAck {
    /// Intent.
    pub intent: OrderIntentId,
    /// State after the submission.
    pub state: OrderIntentState,
}

/// A fill applied by the gateway, for the Position Manager.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FillReport {
    /// The filled intent.
    pub intent: OrderIntent,
    /// Filled quantity.
    pub quantity: Quantity,
    /// Fill price.
    pub price: Price,
    /// Fill time.
    pub at: DateTime<Utc>,
    /// Whether the intent is now completely filled.
    pub complete: bool,
    /// OCO siblings the gateway cancelled because this order filled.
    pub cancelled_siblings: Vec<OrderIntentId>,
}

#[derive(Clone, Debug)]
struct IntentRecord {
    intent: OrderIntent,
    state: OrderIntentState,
    filled: Quantity,
}

impl IntentRecord {
    fn remaining(&self) -> Quantity {
        let rest = self.intent.quantity().value() - self.filled.value();
        Quantity::new(rest).unwrap_or(Quantity::ZERO)
    }

    const fn is_working(&self) -> bool {
        matches!(
            self.state,
            OrderIntentState::Created
                | OrderIntentState::PendingSubmit
                | OrderIntentState::Submitted
                | OrderIntentState::PartiallyFilled
                | OrderIntentState::Unknown
        )
    }
}

#[derive(Default)]
struct GatewayState {
    intents: HashMap<OrderIntentId, IntentRecord>,
    open: HashMap<(InstrumentId, Side), Quantity>,
    journal_failure: bool,
}

/// The Order Gateway.
pub struct OrderGateway {
    account: GatewayAccount,
    executor: Arc<dyn BrokerOrderExecutor>,
    journal: Arc<dyn Journal>,
    halts: Arc<dyn HaltStore>,
    clock: Arc<dyn Clock>,
    live: LivePolicy,
    instruments: HashMap<InstrumentId, InstrumentSpec>,
    state: Mutex<GatewayState>,
}

impl std::fmt::Debug for OrderGateway {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OrderGateway")
            .field("account", &self.account)
            .field("live", &self.live)
            .finish_non_exhaustive()
    }
}

fn terms_error(detail: &str) -> GatewayRejection {
    GatewayRejection::InvalidTerms(detail.to_owned())
}

impl OrderGateway {
    /// Creates the gateway. Only the binaries call this, handing it the one
    /// order-placing executor (INV-01).
    #[must_use]
    pub fn new(
        account: GatewayAccount,
        executor: Arc<dyn BrokerOrderExecutor>,
        journal: Arc<dyn Journal>,
        halts: Arc<dyn HaltStore>,
        clock: Arc<dyn Clock>,
        live: LivePolicy,
        instruments: Vec<InstrumentSpec>,
    ) -> Self {
        Self {
            account,
            executor,
            journal,
            halts,
            clock,
            live,
            instruments: instruments.into_iter().map(|s| (s.id, s)).collect(),
            state: Mutex::new(GatewayState::default()),
        }
    }

    /// Rebuilds intents and the open-quantity ledger from restored journal
    /// state. Nothing is sent: restored intents were sent before they were
    /// journaled as accepted. Intents of other accounts are ignored.
    pub fn restore(&self, restored: &crate::restore::RestoredState) {
        let mut state = self.lock();
        let mut totals: HashMap<(InstrumentId, Side), rust_decimal::Decimal> = HashMap::new();
        for r in restored
            .intents
            .iter()
            .filter(|r| r.intent.account() == self.account.id)
        {
            let total = totals
                .entry((r.intent.instrument(), r.intent.action().side()))
                .or_default();
            match r.intent.risk_effect() {
                RiskEffect::Increasing => *total += r.filled.value(),
                RiskEffect::Reducing => *total -= r.filled.value(),
            }
            state.intents.insert(
                r.intent.id(),
                IntentRecord {
                    intent: r.intent.clone(),
                    state: r.state,
                    filled: r.filled,
                },
            );
        }
        for (key, total) in totals {
            // A negative total cannot come from the gateway's own ledger; the
            // restore consistency check reports it before trading resumes.
            state
                .open
                .insert(key, Quantity::new(total).unwrap_or(Quantity::ZERO));
        }
    }

    /// Intents in the `Unknown` state (outcome not known; reconcile them).
    #[must_use]
    pub fn unknown_intents(&self) -> Vec<OrderIntent> {
        self.lock()
            .intents
            .values()
            .filter(|r| r.state == OrderIntentState::Unknown)
            .map(|r| r.intent.clone())
            .collect()
    }

    fn lock(&self) -> MutexGuard<'_, GatewayState> {
        // A poisoned lock means a panic mid-update; the state is still the
        // best record we have, and every caller re-validates.
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The account this gateway serves.
    #[must_use]
    pub const fn account(&self) -> GatewayAccount {
        self.account
    }

    /// Whether a journal failure has halted new entries (INV-05).
    #[must_use]
    pub fn entries_halted_by_journal_failure(&self) -> bool {
        self.lock().journal_failure
    }

    /// Filled open quantity per the gateway's own ledger.
    #[must_use]
    pub fn open_quantity(&self, instrument: InstrumentId, side: Side) -> Quantity {
        self.lock()
            .open
            .get(&(instrument, side))
            .copied()
            .unwrap_or(Quantity::ZERO)
    }

    /// Current state of an intent.
    #[must_use]
    pub fn intent_state(&self, id: OrderIntentId) -> Option<OrderIntentState> {
        self.lock().intents.get(&id).map(|r| r.state)
    }

    /// Working intents for a position.
    #[must_use]
    pub fn working_intents(&self, position: PositionId) -> Vec<OrderIntent> {
        self.lock()
            .intents
            .values()
            .filter(|r| r.is_working() && r.intent.position() == Some(position))
            .map(|r| r.intent.clone())
            .collect()
    }

    async fn halt_state(&self) -> HaltState {
        match self.halts.load().await {
            Ok(halts) => HaltState::Known(halts),
            Err(_) => HaltState::Unknown,
        }
    }

    async fn journal(&self, entry: &JournalEntry) -> bool {
        if self.journal.append(entry).await.is_ok() {
            true
        } else {
            self.lock().journal_failure = true;
            false
        }
    }

    fn validate_terms(intent: &OrderIntent, spec: &InstrumentSpec) -> Result<(), GatewayRejection> {
        let terms = intent.terms();
        let caps = &spec.capabilities;
        if !caps.order_types.contains(&terms.order_type) {
            return Err(terms_error("order type not allowed for the instrument"));
        }
        if !caps.products.contains(&terms.product) {
            return Err(terms_error("product not allowed for the instrument"));
        }
        if !caps.validities.contains(&terms.validity) {
            return Err(terms_error("validity not allowed for the instrument"));
        }
        let needs_limit = matches!(terms.order_type, OrderType::Limit | OrderType::StopLimit);
        let needs_trigger = matches!(
            terms.order_type,
            OrderType::StopMarket | OrderType::StopLimit
        );
        if needs_limit != terms.limit.is_some() || needs_trigger != terms.trigger.is_some() {
            return Err(terms_error("prices do not match the order type"));
        }
        for price in [terms.limit, terms.trigger].into_iter().flatten() {
            if !spec.is_tick_aligned(price) {
                return Err(terms_error("price is not a whole number of ticks"));
            }
        }
        if terms.order_type == OrderType::Market && !caps.supports_market_orders {
            return Err(terms_error("market orders not supported"));
        }
        Ok(())
    }

    /// Quantity already committed to working risk-reducing orders on this side.
    fn committed_exits(state: &GatewayState, intent: &OrderIntent) -> Quantity {
        let side = intent.action().side();
        let mut by_group: HashMap<Option<PositionId>, rust_decimal::Decimal> = HashMap::new();
        let mut ungrouped = rust_decimal::Decimal::ZERO;
        for r in state.intents.values() {
            let same = r.intent.instrument() == intent.instrument()
                && r.intent.action().side() == side
                && r.intent.risk_effect() == RiskEffect::Reducing
                && r.is_working()
                && r.intent.id() != intent.id();
            if !same {
                continue;
            }
            let remaining = r.remaining().value();
            match r.intent.oco_group() {
                Some(group) if Some(group) == intent.oco_group() => {} // the new order shares this group
                Some(group) => {
                    let entry = by_group.entry(Some(group)).or_default();
                    *entry = (*entry).max(remaining);
                }
                None => ungrouped += remaining,
            }
        }
        let total = ungrouped + by_group.values().copied().sum::<rust_decimal::Decimal>();
        Quantity::new(total).unwrap_or(Quantity::ZERO)
    }

    /// Submits an intent. See the module docs for the checks.
    pub async fn submit(
        &self,
        intent: OrderIntent,
        stage: Option<StrategyStage>,
    ) -> Result<GatewayAck, GatewayRejection> {
        let mut results = self.submit_group(vec![intent], stage).await?;
        results
            .pop()
            .unwrap_or(Err(GatewayRejection::UnknownIntent))
    }

    /// Submits the legs of one OCO group (a protective stop and its target)
    /// as one broker order, so a broker with native OCO orders (a two-leg
    /// GTT) never holds one leg without the other.
    ///
    /// Every leg must be risk-reducing and carry the same OCO group. All
    /// legs are validated and journaled before anything is sent; if any leg
    /// fails validation, none is sent. Returns one result per leg, in order.
    pub async fn submit_oco(
        &self,
        legs: Vec<OrderIntent>,
        stage: Option<StrategyStage>,
    ) -> Result<Vec<Result<GatewayAck, GatewayRejection>>, GatewayRejection> {
        let group = legs.first().and_then(OrderIntent::oco_group);
        let well_formed = legs.len() >= 2
            && group.is_some()
            && legs
                .iter()
                .all(|l| l.oco_group() == group && l.risk_effect() == RiskEffect::Reducing);
        if !well_formed {
            return Err(terms_error(
                "an OCO submission needs two or more risk-reducing legs of one group",
            ));
        }
        self.submit_group(legs, stage).await
    }

    async fn submit_group(
        &self,
        intents: Vec<OrderIntent>,
        stage: Option<StrategyStage>,
    ) -> Result<Vec<Result<GatewayAck, GatewayRejection>>, GatewayRejection> {
        // 1. Idempotency and reservation.
        {
            let mut state = self.lock();
            let known: Vec<GatewayAck> = intents
                .iter()
                .filter_map(|i| {
                    state.intents.get(&i.id()).map(|r| GatewayAck {
                        intent: i.id(),
                        state: r.state,
                    })
                })
                .collect();
            if known.len() == intents.len() {
                return Ok(known.into_iter().map(Ok).collect());
            }
            if !known.is_empty() {
                // Part of the group was submitted before: sending the rest
                // would split the group.
                return Err(terms_error("part of the OCO group was already submitted"));
            }
            for intent in &intents {
                state.intents.insert(
                    intent.id(),
                    IntentRecord {
                        intent: intent.clone(),
                        state: OrderIntentState::Created,
                        filled: Quantity::ZERO,
                    },
                );
            }
        }
        // 2-4. Validation: all legs or none.
        let mut failure = None;
        for intent in &intents {
            if let Err(rejection) = self.validate(intent, stage).await {
                failure = Some(rejection);
                break;
            }
        }
        if let Some(rejection) = failure {
            for intent in &intents {
                self.set_state(intent.id(), OrderIntentState::GatewayRejected);
                // Best effort: the order was never sent, so a failed write loses nothing.
                let _ = self
                    .journal
                    .append(&JournalEntry::OrderEvent {
                        intent: intent.id(),
                        event: OrderEvent::GatewayRejected {
                            reason: rejection.to_string(),
                        },
                        state: OrderIntentState::GatewayRejected,
                        at: self.clock.now(),
                    })
                    .await;
            }
            return Err(rejection);
        }

        // 5. Journal before send.
        let now = self.clock.now();
        let mut journaled = true;
        for intent in &intents {
            journaled = journaled
                && self
                    .journal(&JournalEntry::OrderIntent {
                        intent: Box::new(intent.clone()),
                    })
                    .await
                && self
                    .journal(&JournalEntry::OrderEvent {
                        intent: intent.id(),
                        event: OrderEvent::GatewayAccepted,
                        state: OrderIntentState::PendingSubmit,
                        at: now,
                    })
                    .await;
        }
        if !journaled {
            for intent in &intents {
                self.set_state(intent.id(), OrderIntentState::GatewayRejected);
            }
            return Err(GatewayRejection::JournalUnavailable);
        }
        for intent in &intents {
            self.set_state(intent.id(), OrderIntentState::PendingSubmit);
        }

        // 6. The broker call.
        let requests: Vec<BrokerOrderRequest> = intents
            .iter()
            .map(|intent| {
                let symbol = self
                    .instruments
                    .get(&intent.instrument())
                    .map_or("", |s| s.symbol.as_str());
                BrokerOrderRequest::from_intent(intent, symbol)
            })
            .collect();
        let mut outcomes = if let [single] = requests.as_slice() {
            vec![self.executor.submit(single).await]
        } else {
            self.executor.submit_oco(&requests).await
        };
        // A broker answer that does not cover every leg leaves the rest unknown.
        outcomes.resize(
            requests.len(),
            Err(BrokerError::Transport(
                "the broker did not answer for this leg".to_owned(),
            )),
        );
        let mut results = Vec::with_capacity(intents.len());
        for (intent, outcome) in intents.iter().zip(outcomes) {
            results.push(self.record_submission(intent.id(), outcome).await);
        }
        Ok(results)
    }

    async fn record_submission(
        &self,
        id: OrderIntentId,
        outcome: Result<crate::ports::BrokerOrderAck, BrokerError>,
    ) -> Result<GatewayAck, GatewayRejection> {
        let (event, next) = match outcome {
            Ok(ack) => (
                OrderEvent::BrokerAcknowledged {
                    broker_order_id: ack.broker_order_id.0,
                },
                OrderIntentState::Submitted,
            ),
            Err(BrokerError::Rejected(reason)) => (
                OrderEvent::BrokerRejected { reason },
                OrderIntentState::BrokerRejected,
            ),
            Err(BrokerError::Transport(detail)) => (
                OrderEvent::TransportFailure { detail },
                OrderIntentState::Unknown,
            ),
        };
        self.set_state(id, next);
        let rejected_reason = match &event {
            OrderEvent::BrokerRejected { reason } => Some(reason.clone()),
            _ => None,
        };
        // The order may now be live; a failed write here latches the entry halt.
        self.journal(&JournalEntry::OrderEvent {
            intent: id,
            event,
            state: next,
            at: self.clock.now(),
        })
        .await;
        match rejected_reason {
            Some(reason) => Err(GatewayRejection::BrokerRejected(reason)),
            None => Ok(GatewayAck {
                intent: id,
                state: next,
            }),
        }
    }

    async fn validate(
        &self,
        intent: &OrderIntent,
        stage: Option<StrategyStage>,
    ) -> Result<(), GatewayRejection> {
        if intent.account() != self.account.id {
            return Err(GatewayRejection::WrongAccount);
        }
        let spec = self
            .instruments
            .get(&intent.instrument())
            .ok_or(GatewayRejection::UnknownInstrument)?;
        if !spec.is_valid_order_quantity(intent.quantity()) {
            return Err(GatewayRejection::InvalidQuantity);
        }
        Self::validate_terms(intent, spec)?;

        match intent.risk_effect() {
            RiskEffect::Increasing => {
                let authorized = intent
                    .authorized_quantity()
                    .ok_or(GatewayRejection::NotAuthorized)?;
                if intent.quantity() > authorized {
                    return Err(GatewayRejection::NotAuthorized);
                }
                if !spec.permits_overnight(intent.action().side()) {
                    return Err(GatewayRejection::ShortNotPermitted);
                }
                if self.lock().journal_failure {
                    return Err(GatewayRejection::JournalUnavailable);
                }
                let context = OrderContext {
                    account: intent.account(),
                    strategy_version: intent.strategy_version(),
                    instrument: intent.instrument(),
                };
                let halts = self.halt_state().await;
                check_order(RiskEffect::Increasing, &halts, &context, self.clock.now())
                    .map_err(GatewayRejection::Halted)?;
            }
            RiskEffect::Reducing => {
                let state = self.lock();
                let open = state
                    .open
                    .get(&(intent.instrument(), intent.action().side()))
                    .copied()
                    .unwrap_or(Quantity::ZERO);
                let committed = Self::committed_exits(&state, intent).value();
                let available = Quantity::new(open.value() - committed).unwrap_or(Quantity::ZERO);
                check_exit_quantity(available, intent.quantity())
                    .map_err(GatewayRejection::ExitExceedsOpen)?;
            }
        }

        if self.account.mode == AccountMode::Live {
            check_live_order(
                &self.live,
                intent.risk_effect(),
                self.account.live_armed,
                stage,
            )
            .map_err(GatewayRejection::LiveNotPermitted)?;
        }
        Ok(())
    }

    fn set_state(&self, id: OrderIntentId, next: OrderIntentState) {
        if let Some(record) = self.lock().intents.get_mut(&id) {
            record.state = next;
        }
    }

    fn transition(
        &self,
        id: OrderIntentId,
        event: &OrderEvent,
    ) -> Result<OrderIntentState, GatewayRejection> {
        let mut state = self.lock();
        let record = state
            .intents
            .get_mut(&id)
            .ok_or(GatewayRejection::UnknownIntent)?;
        let next = record
            .state
            .apply(event)
            .map_err(|e| GatewayRejection::InvalidEvent(e.to_string()))?;
        record.state = next;
        Ok(next)
    }

    /// Applies a broker fill: updates the intent and the open-quantity ledger,
    /// journals it, and cancels OCO siblings once a group member fills completely.
    pub async fn on_fill(&self, fill: BrokerFill) -> Result<FillReport, GatewayRejection> {
        let (intent, complete, siblings) = {
            let mut state = self.lock();
            let record = state
                .intents
                .get(&fill.client_order_id)
                .cloned()
                .ok_or(GatewayRejection::UnknownIntent)?;
            if fill.quantity.is_zero() || fill.quantity > record.remaining() {
                return Err(GatewayRejection::InvalidEvent(
                    "fill quantity exceeds the remaining quantity".to_owned(),
                ));
            }
            let filled = Quantity::new(record.filled.value() + fill.quantity.value())
                .map_err(|e| GatewayRejection::InvalidEvent(e.to_string()))?;
            let complete = filled == record.intent.quantity();
            let event = if complete {
                OrderEvent::Filled
            } else {
                OrderEvent::PartiallyFilled
            };
            let next = record
                .state
                .apply(&event)
                .map_err(|e| GatewayRejection::InvalidEvent(e.to_string()))?;
            let key = (record.intent.instrument(), record.intent.action().side());
            let open = state
                .open
                .get(&key)
                .copied()
                .unwrap_or(Quantity::ZERO)
                .value();
            let updated = match record.intent.action() {
                TradeAction::OpenLong | TradeAction::OpenShort => open + fill.quantity.value(),
                TradeAction::CloseLong | TradeAction::CloseShort => open - fill.quantity.value(),
            };
            let updated = Quantity::new(updated).map_err(|_| {
                GatewayRejection::InvalidEvent("fill would reverse the position".to_owned())
            })?;
            state.open.insert(key, updated);
            if let Some(r) = state.intents.get_mut(&fill.client_order_id) {
                r.filled = filled;
                r.state = next;
            }
            let siblings: Vec<OrderIntentId> = match (complete, record.intent.oco_group()) {
                (true, Some(group)) => state
                    .intents
                    .values()
                    .filter(|r| {
                        r.intent.oco_group() == Some(group)
                            && r.intent.id() != record.intent.id()
                            && r.is_working()
                    })
                    .map(|r| r.intent.id())
                    .collect(),
                _ => Vec::new(),
            };
            (record.intent, complete, siblings)
        };
        self.journal(&JournalEntry::Fill {
            intent: fill.client_order_id,
            quantity: fill.quantity,
            price: fill.price,
            at: fill.at,
        })
        .await;
        let mut cancelled = Vec::new();
        for sibling in siblings {
            if self.cancel(sibling).await.is_ok() {
                cancelled.push(sibling);
            }
        }
        Ok(FillReport {
            intent,
            quantity: fill.quantity,
            price: fill.price,
            at: fill.at,
            complete,
            cancelled_siblings: cancelled,
        })
    }

    /// Cancels a working intent at the broker. Cancelling is never blocked by a halt.
    pub async fn cancel(&self, id: OrderIntentId) -> Result<(), GatewayRejection> {
        let working = self
            .lock()
            .intents
            .get(&id)
            .map(IntentRecord::is_working)
            .ok_or(GatewayRejection::UnknownIntent)?;
        if !working {
            return Ok(());
        }
        match self.executor.cancel(id).await {
            Ok(()) => self
                .record_event(id, OrderEvent::Cancelled)
                .await
                .map(|_| ()),
            Err(BrokerError::Rejected(reason)) => Err(GatewayRejection::BrokerRejected(reason)),
            Err(BrokerError::Transport(detail)) => self
                .record_event(id, OrderEvent::TransportFailure { detail })
                .await
                .map(|_| ()),
        }
    }

    /// Records that an order expired at the end of its validity.
    pub async fn on_expired(&self, id: OrderIntentId) -> Result<(), GatewayRejection> {
        self.record_event(id, OrderEvent::Expired).await.map(|_| ())
    }

    /// Resolves an `Unknown` intent from the broker's records (reconciliation only).
    pub async fn reconcile_intent(
        &self,
        id: OrderIntentId,
        observed: ReconciledState,
    ) -> Result<OrderIntentState, GatewayRejection> {
        self.record_event(id, OrderEvent::Reconciled { observed })
            .await
    }

    async fn record_event(
        &self,
        id: OrderIntentId,
        event: OrderEvent,
    ) -> Result<OrderIntentState, GatewayRejection> {
        let next = self.transition(id, &event)?;
        self.journal(&JournalEntry::OrderEvent {
            intent: id,
            event,
            state: next,
            at: self.clock.now(),
        })
        .await;
        Ok(next)
    }
}
