//! Position Manager (spec §5.2): fills, protection, exits and reconciliation.
//!
//! - After an entry fills it places a protective stop and a target as one OCO
//!   group through the Order Gateway. If protection cannot be placed the
//!   position is `Unprotected` and the caller must alert (it then counts at
//!   the gap shock in open risk).
//! - On each completed bar it checks the time exit and invalidation rules and
//!   exits at the next open with a market order, after cancelling protection.
//! - Reconciliation compares its book with the broker's and reports
//!   mismatches. It never trades to "fix" one.
//!
//! Every position transition is journaled.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use chrono::{DateTime, NaiveDate, Utc};
use qd_domain::action::{RiskEffect, Side};
use qd_domain::ids::{
    AccountId, DecisionId, InstrumentId, OrderIntentId, PositionId, StrategyVersionId,
};
use qd_domain::instrument::{InstrumentSpec, OrderType, ProductType, Validity};
use qd_domain::lifecycle::position::{PositionEvent, PositionState};
use qd_domain::lifecycle::strategy::StrategyStage;
use qd_domain::market::Bar;
use qd_domain::num::{Price, Quantity};
use qd_domain::outcome::ExitReason;
use qd_domain::plan::InvalidationRule;
use qd_domain::proposal::TradeProposal;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::gateway::{FillReport, GatewayRejection, OrderGateway};
use crate::journal::JournalEntry;
use crate::orders::{EntryAuthorization, OrderIntent, OrderPurpose, OrderTerms};
use crate::ports::{BrokerPosition, Clock, Journal};

/// One position. Every change is journaled as a full snapshot, so the book
/// can be rebuilt from the journal after a restart.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Position {
    /// Position id.
    pub id: PositionId,
    /// Account.
    pub account: AccountId,
    /// Instrument.
    pub instrument: InstrumentId,
    /// Strategy version.
    pub strategy_version: Option<StrategyVersionId>,
    /// Stage of the version when opened, for live-gate checks on its orders.
    pub stage: Option<StrategyStage>,
    /// Decision that opened it.
    pub decision: DecisionId,
    /// Setup type of the opening proposal (evidence is kept per setup type).
    #[serde(default)]
    pub setup_type: String,
    /// Long or short.
    pub side: Side,
    /// Lifecycle state.
    pub state: PositionState,
    /// Open quantity.
    pub quantity: Quantity,
    /// Quantity the Risk Gate approved for the entry.
    pub planned_quantity: Quantity,
    /// Total quantity ever filled on entry.
    pub entered_quantity: Quantity,
    /// Average entry price.
    pub entry_price: Option<Price>,
    /// Planned stop.
    pub stop: Price,
    /// Planned target.
    pub target: Price,
    /// Maximum holding period in trading days.
    pub max_holding_days: u16,
    /// Invalidation rules.
    pub invalidation: Vec<InvalidationRule>,
    /// `risk_net` per unit at entry, instrument currency (for the R-multiple).
    pub risk_net_per_unit: Decimal,
    /// Contract multiplier.
    pub multiplier: Decimal,
    /// Trading date the entry filled.
    pub opened_on: Option<NaiveDate>,
    /// Completed bars held since the entry bar.
    pub bars_held: u16,
    /// The last bar counted in `bars_held` (makes re-processing a day idempotent).
    #[serde(default)]
    pub last_bar_counted: Option<NaiveDate>,
    /// Realized P&L, instrument currency, before costs.
    pub realized_gross: Decimal,
    /// Why it closed.
    pub exit_reason: Option<ExitReason>,
    /// Product.
    pub product: ProductType,
}

/// A closed trade, for review and backtest metrics.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ClosedTrade {
    /// The position as it closed.
    pub position: Position,
}

/// What the caller must act on after a Position Manager step.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct PositionUpdate {
    /// Positions left without valid protection: alert.
    pub unprotected: Vec<PositionId>,
    /// Positions that closed.
    pub closed: Vec<PositionId>,
    /// Gateway refusals encountered.
    pub rejections: Vec<String>,
}

/// A difference between the book and the broker.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReconciliationMismatch {
    /// Instrument.
    pub instrument: InstrumentId,
    /// Signed quantity per the book (long positive).
    pub book: Decimal,
    /// Signed quantity per the broker.
    pub broker: Decimal,
}

/// The Position Manager.
pub struct PositionManager {
    gateway: Arc<OrderGateway>,
    journal: Arc<dyn Journal>,
    clock: Arc<dyn Clock>,
    positions: Mutex<HashMap<PositionId, Position>>,
    by_intent: Mutex<HashMap<OrderIntentId, PositionId>>,
}

impl std::fmt::Debug for PositionManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PositionManager").finish_non_exhaustive()
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl PositionManager {
    /// Creates the manager over the gateway (its only way to place orders).
    #[must_use]
    pub fn new(
        gateway: Arc<OrderGateway>,
        journal: Arc<dyn Journal>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            gateway,
            journal,
            clock,
            positions: Mutex::new(HashMap::new()),
            by_intent: Mutex::new(HashMap::new()),
        }
    }

    /// All positions, open and closed.
    #[must_use]
    pub fn positions(&self) -> Vec<Position> {
        let mut all: Vec<Position> = lock(&self.positions).values().cloned().collect();
        all.sort_by_key(|p| p.id);
        all
    }

    /// Positions not yet closed.
    #[must_use]
    pub fn active(&self) -> Vec<Position> {
        self.positions()
            .into_iter()
            .filter(|p| p.state != PositionState::Closed)
            .collect()
    }

    /// Whether an active position exists on the instrument.
    #[must_use]
    pub fn has_active(&self, instrument: InstrumentId) -> bool {
        lock(&self.positions)
            .values()
            .any(|p| p.instrument == instrument && p.state != PositionState::Closed)
    }

    async fn transition(&self, id: PositionId, event: PositionEvent) -> Option<PositionState> {
        let next = {
            let mut positions = lock(&self.positions);
            let position = positions.get_mut(&id)?;
            let next = position.state.apply(&event).ok()?;
            position.state = next;
            if let PositionEvent::ExitStarted { reason } = &event {
                position.exit_reason = Some(*reason);
            }
            next
        };
        // Best effort for position events: the orders themselves were journaled
        // by the gateway before any broker call. A lost snapshot makes the
        // restored book disagree with the order ledger, and a restore then
        // fails closed (ADR 0009).
        let _ = self
            .journal
            .append(&JournalEntry::PositionEvent {
                position: id,
                event,
                state: next,
                at: self.clock.now(),
            })
            .await;
        self.snapshot(id).await;
        Some(next)
    }

    /// Journals the full current state of a position.
    async fn snapshot(&self, id: PositionId) {
        let Some(position) = lock(&self.positions).get(&id).cloned() else {
            return;
        };
        let _ = self
            .journal
            .append(&JournalEntry::PositionSnapshot {
                position: Box::new(position),
                at: self.clock.now(),
            })
            .await;
    }

    /// Rebuilds the book from restored journal state. Call once, before use,
    /// after [`OrderGateway::restore`].
    pub fn restore(&self, restored: &crate::restore::RestoredState) {
        let mut positions = lock(&self.positions);
        let mut by_intent = lock(&self.by_intent);
        for p in &restored.positions {
            positions.insert(p.id, p.clone());
        }
        for r in &restored.intents {
            if let Some(position) = r.intent.position() {
                by_intent.insert(r.intent.id(), position);
            }
        }
    }

    /// Opens a position: records it and submits the entry through the gateway.
    pub async fn open(
        &self,
        authorization: &EntryAuthorization,
        proposal: &TradeProposal,
        spec: &InstrumentSpec,
        stage: StrategyStage,
        product: ProductType,
    ) -> Result<PositionId, GatewayRejection> {
        let now = self.clock.now();
        let id = PositionId::new_at(now);
        let plan = proposal.plan();
        let entry = plan.entry();
        let order_type = match entry.order_type {
            qd_domain::plan::EntryOrderType::Limit => OrderType::Limit,
            qd_domain::plan::EntryOrderType::StopLimit => OrderType::StopLimit,
            qd_domain::plan::EntryOrderType::Market => OrderType::Market,
        };
        let terms = OrderTerms {
            order_type,
            limit: (order_type != OrderType::Market).then_some(entry.price),
            trigger: (order_type == OrderType::StopLimit).then_some(entry.price),
            validity: Validity::Day,
            product,
        };
        let intent = OrderIntent::entry(OrderIntentId::new_at(now), authorization, id, terms, now);
        let position = Position {
            id,
            account: authorization.account(),
            instrument: authorization.instrument(),
            strategy_version: authorization.strategy_version(),
            stage: Some(stage),
            decision: authorization.decision(),
            setup_type: proposal.setup_type().to_owned(),
            side: authorization.action().side(),
            state: PositionState::Opening,
            quantity: Quantity::ZERO,
            planned_quantity: authorization.quantity(),
            entered_quantity: Quantity::ZERO,
            entry_price: None,
            stop: plan.stop(),
            target: plan.target(),
            max_holding_days: plan.max_holding_days(),
            invalidation: plan.invalidation().to_vec(),
            risk_net_per_unit: proposal.economics().risk_net(),
            multiplier: spec.multiplier,
            opened_on: None,
            bars_held: 0,
            last_bar_counted: None,
            realized_gross: Decimal::ZERO,
            exit_reason: None,
            product,
        };
        lock(&self.positions).insert(id, position);
        lock(&self.by_intent).insert(intent.id(), id);
        self.snapshot(id).await;
        match self.gateway.submit(intent, Some(stage)).await {
            Ok(_) => Ok(id),
            Err(rejection) => {
                self.transition(id, PositionEvent::EntryAbandoned).await;
                Err(rejection)
            }
        }
    }

    /// An entry order ended without (further) fills: expired, cancelled or rejected.
    pub async fn on_entry_ended(&self, intent: OrderIntentId) {
        let Some(id) = lock(&self.by_intent).get(&intent).copied() else {
            return;
        };
        let opening = lock(&self.positions)
            .get(&id)
            .is_some_and(|p| p.state == PositionState::Opening);
        if opening {
            self.transition(id, PositionEvent::EntryAbandoned).await;
        }
    }

    /// Applies a fill the gateway accepted.
    pub async fn on_fill(&self, report: &FillReport, trading_date: NaiveDate) -> PositionUpdate {
        let mut update = PositionUpdate::default();
        let Some(id) = report.intent.position() else {
            return update;
        };
        let fill_value = report.quantity.value() * report.price.value();
        match report.intent.risk_effect() {
            RiskEffect::Increasing => {
                let first = {
                    let mut positions = lock(&self.positions);
                    let Some(p) = positions.get_mut(&id) else {
                        return update;
                    };
                    let old_value = p
                        .entry_price
                        .map_or(Decimal::ZERO, |e| e.value() * p.quantity.value());
                    let quantity = p.quantity.value() + report.quantity.value();
                    p.entry_price = Price::new((old_value + fill_value) / quantity).ok();
                    p.quantity = Quantity::new(quantity).unwrap_or(p.quantity);
                    p.entered_quantity =
                        Quantity::new(p.entered_quantity.value() + report.quantity.value())
                            .unwrap_or(p.entered_quantity);
                    let first = p.state == PositionState::Opening;
                    if first {
                        p.opened_on = Some(trading_date);
                    }
                    first
                };
                if first {
                    self.transition(id, PositionEvent::EntryFilled).await;
                } else {
                    self.snapshot(id).await;
                }
                if report.complete {
                    self.protect(id, &mut update).await;
                }
            }
            RiskEffect::Reducing => {
                let reason = match report.intent.purpose() {
                    OrderPurpose::ProtectiveStop => ExitReason::StopHit,
                    OrderPurpose::Target => ExitReason::TargetHit,
                    OrderPurpose::Exit { reason } => reason,
                    OrderPurpose::Entry { .. } => ExitReason::Manual,
                };
                let (remaining, state) = {
                    let mut positions = lock(&self.positions);
                    let Some(p) = positions.get_mut(&id) else {
                        return update;
                    };
                    let entry = p.entry_price.map_or(Decimal::ZERO, Price::value);
                    let per_unit = match p.side {
                        Side::Long => report.price.value() - entry,
                        Side::Short => entry - report.price.value(),
                    };
                    p.realized_gross += per_unit * report.quantity.value() * p.multiplier;
                    p.quantity = Quantity::new(p.quantity.value() - report.quantity.value())
                        .unwrap_or(Quantity::ZERO);
                    (p.quantity, p.state)
                };
                if state == PositionState::Exiting {
                    self.snapshot(id).await;
                } else {
                    self.transition(id, PositionEvent::ExitStarted { reason })
                        .await;
                }
                if remaining.is_zero() {
                    self.transition(id, PositionEvent::ExitCompleted).await;
                    update.closed.push(id);
                }
            }
        }
        update
    }

    async fn protect(&self, id: PositionId, update: &mut PositionUpdate) {
        let Some(p) = lock(&self.positions).get(&id).cloned() else {
            return;
        };
        let now = self.clock.now();
        let exit = p.side.exit();
        let stop_terms = OrderTerms {
            order_type: OrderType::StopMarket,
            limit: None,
            trigger: Some(p.stop),
            validity: Validity::GoodTillCancelled,
            product: p.product,
        };
        let target_terms = OrderTerms {
            order_type: OrderType::Limit,
            limit: Some(p.target),
            trigger: None,
            validity: Validity::GoodTillCancelled,
            product: p.product,
        };
        let leg = |terms: OrderTerms, purpose: OrderPurpose| {
            let intent = OrderIntent::reducing(
                OrderIntentId::new_at(now),
                p.account,
                p.instrument,
                p.strategy_version,
                id,
                exit,
                p.quantity,
                terms,
                purpose,
                Some(id),
                now,
            );
            lock(&self.by_intent).insert(intent.id(), id);
            intent
        };
        let legs = vec![
            leg(stop_terms, OrderPurpose::ProtectiveStop),
            leg(target_terms, OrderPurpose::Target),
        ];
        let purposes: Vec<OrderPurpose> = legs.iter().map(OrderIntent::purpose).collect();
        let mut protected = true;
        match self.gateway.submit_oco(legs, p.stage).await {
            Ok(results) => {
                for (purpose, result) in purposes.into_iter().zip(results) {
                    if let Err(rejection) = result {
                        update.rejections.push(rejection.to_string());
                        if purpose == OrderPurpose::ProtectiveStop {
                            protected = false;
                        }
                    }
                }
            }
            Err(rejection) => {
                // The pair was refused before anything was sent. The stop
                // matters more than the target: try it on its own.
                update.rejections.push(rejection.to_string());
                if let Err(rejection) = self
                    .gateway
                    .submit(leg(stop_terms, OrderPurpose::ProtectiveStop), p.stage)
                    .await
                {
                    update.rejections.push(rejection.to_string());
                    protected = false;
                }
            }
        }
        let event = if protected {
            PositionEvent::ProtectionConfirmed
        } else {
            update.unprotected.push(id);
            PositionEvent::ProtectionLost
        };
        self.transition(id, event).await;
    }

    /// End-of-bar checks for one instrument: time exit and invalidation.
    pub async fn on_bar_close(&self, instrument: InstrumentId, bar: &Bar) -> PositionUpdate {
        let mut update = PositionUpdate::default();
        let candidates: Vec<Position> = {
            let mut positions = lock(&self.positions);
            positions
                .values_mut()
                .filter(|p| {
                    p.instrument == instrument
                        && matches!(
                            p.state,
                            PositionState::Open
                                | PositionState::Protected
                                | PositionState::Unprotected
                        )
                })
                .map(|p| {
                    if p.opened_on.is_some_and(|d| bar.date() > d)
                        && p.last_bar_counted.is_none_or(|last| bar.date() > last)
                    {
                        p.last_bar_counted = Some(bar.date());
                        p.bars_held = p.bars_held.saturating_add(1);
                    }
                    p.clone()
                })
                .collect()
        };
        for p in &candidates {
            self.snapshot(p.id).await;
        }
        for p in candidates {
            let close = bar.close();
            let invalidated = p.invalidation.iter().any(|rule| match rule {
                InvalidationRule::CloseBeyond { level } => match p.side {
                    Side::Long => close < *level,
                    Side::Short => close > *level,
                },
                InvalidationRule::RegimeChange | InvalidationRule::Custom { .. } => false,
            });
            let reason = if p.bars_held >= p.max_holding_days {
                Some(ExitReason::TimeExit)
            } else if invalidated {
                Some(ExitReason::Invalidated)
            } else {
                None
            };
            if let Some(reason) = reason {
                self.exit(p.id, reason, &mut update).await;
            }
        }
        update
    }

    /// Exits a position at market: cancels its protection, then sells the
    /// open quantity. Never blocked by halts (INV-02).
    pub async fn exit(&self, id: PositionId, reason: ExitReason, update: &mut PositionUpdate) {
        let Some(p) = lock(&self.positions).get(&id).cloned() else {
            return;
        };
        if p.quantity.is_zero() {
            return;
        }
        for working in self.gateway.working_intents(id) {
            if working.risk_effect() == RiskEffect::Reducing {
                if let Err(rejection) = self.gateway.cancel(working.id()).await {
                    update.rejections.push(rejection.to_string());
                }
            }
        }
        let now = self.clock.now();
        let intent = OrderIntent::reducing(
            OrderIntentId::new_at(now),
            p.account,
            p.instrument,
            p.strategy_version,
            id,
            p.side.exit(),
            p.quantity,
            OrderTerms {
                order_type: OrderType::Market,
                limit: None,
                trigger: None,
                validity: Validity::Day,
                product: p.product,
            },
            OrderPurpose::Exit { reason },
            None,
            now,
        );
        lock(&self.by_intent).insert(intent.id(), id);
        match self.gateway.submit(intent, p.stage).await {
            Ok(_) => {
                self.transition(id, PositionEvent::ExitStarted { reason })
                    .await;
            }
            Err(rejection) => {
                update.rejections.push(rejection.to_string());
                update.unprotected.push(id);
            }
        }
    }

    /// Flattens every active position (for example on demotion or an owner request).
    pub async fn flatten_all(&self, reason: ExitReason) -> PositionUpdate {
        let mut update = PositionUpdate::default();
        for p in self.active() {
            self.exit(p.id, reason, &mut update).await;
        }
        update
    }

    /// Compares the book with the broker's net positions. Reports mismatches;
    /// never trades to fix them.
    #[must_use]
    pub fn reconcile(&self, broker: &[BrokerPosition]) -> Vec<ReconciliationMismatch> {
        reconcile_book(&self.active(), broker)
    }

    /// Time of the manager's clock (for callers without their own).
    #[must_use]
    pub fn now(&self) -> DateTime<Utc> {
        self.clock.now()
    }
}

/// Compares a book of positions with the broker's net positions.
#[must_use]
pub fn reconcile_book(
    positions: &[Position],
    broker: &[BrokerPosition],
) -> Vec<ReconciliationMismatch> {
    let mut book: HashMap<InstrumentId, Decimal> = HashMap::new();
    for p in positions
        .iter()
        .filter(|p| p.state != PositionState::Closed)
    {
        let signed = match p.side {
            Side::Long => p.quantity.value(),
            Side::Short => -p.quantity.value(),
        };
        *book.entry(p.instrument).or_default() += signed;
    }
    let mut broker_map: HashMap<InstrumentId, Decimal> = HashMap::new();
    for b in broker {
        *broker_map.entry(b.instrument).or_default() += b.net_quantity;
    }
    let mut instruments: Vec<InstrumentId> =
        book.keys().chain(broker_map.keys()).copied().collect();
    instruments.sort();
    instruments.dedup();
    instruments
        .into_iter()
        .filter_map(|instrument| {
            let b = book.get(&instrument).copied().unwrap_or_default();
            let r = broker_map.get(&instrument).copied().unwrap_or_default();
            (b != r).then_some(ReconciliationMismatch {
                instrument,
                book: b,
                broker: r,
            })
        })
        .collect()
}
