//! The daily trading cycle shared by backtests and paper trading (INV-08).
//!
//! For one trading date, per instrument with a bar on that date:
//!
//! 1. The simulated venue processes the bar; fills go through the Order
//!    Gateway to the Position Manager.
//! 2. Protective stops placed on the entry bar are checked against that bar.
//! 3. The Position Manager applies the time exit and invalidation rules.
//!
//! Then closed trades are booked with their costs, equity is marked to the
//! close, halt triggers run, the book is reconciled with the venue, and every
//! instrument without an active position is evaluated by every strategy
//! slot: strategy → Decision Engine (journaled) → Position Manager → Order
//! Gateway. The day ends with a `day_closed` journal entry holding the
//! account book, which a paper restart restores (ADR 0009).
//!
//! Two clocks: the market clock is set to each date's close and stamps
//! orders (so a venue knows which bar an order was placed on); the wall
//! clock decides whether halts are active and stamps decisions. In a
//! backtest both are the same simulated clock. In paper trading the wall
//! clock is real time, so a halt created today also blocks a catch-up run
//! over earlier dates.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Datelike, NaiveDate, NaiveTime, TimeZone, Utc};
use qd_domain::action::Side;
use qd_domain::costs::{CostModel, CostRequest};
use qd_domain::halt::HaltState;
use qd_domain::ids::{
    AccountId, DecisionId, HaltId, InstrumentId, OrderIntentId, PositionId, SnapshotId,
};
use qd_domain::instrument::{InstrumentSpec, ProductType};
use qd_domain::lifecycle::order::ReconciledState;
use qd_domain::lifecycle::position::PositionState;
use qd_domain::market::{Bar, BarSeries};
use qd_domain::num::{Currency, FxRate, Money, Price, Quantity};
use qd_domain::outcome::{DecisionOutcome, ExitReason};
use qd_domain::portfolio::{PositionExposure, Protection, position_open_risk};
use qd_domain::proposal::AccountMode;
use qd_risk::config::RiskConfig;
use qd_risk::gate::{AccountRiskState, RiskItem};
use qd_risk::triggers::halt_triggers;
use qd_strategy::features::FeatureSet;
use qd_strategy::regime::RegimeClassifier;
use qd_strategy::strategy::{Evaluation, SetupCandidate, Strategy, StrategyOutput, run_strategy};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::decision::{
    AgentForecast, DecisionContext, DecisionEngine, JournaledDecision, StrategyVersionInfo,
};
use crate::gateway::{GatewayRejection, OrderGateway};
use crate::journal::JournalEntry;
use crate::ports::{BrokerAccountReader, BrokerFill, Clock, HaltStore, Journal};
use crate::positions::{Position, PositionManager, PositionUpdate};
use crate::restore::RestoredState;

/// A clock moved forward by the caller: each trading date's close.
#[derive(Debug)]
pub struct SessionClock {
    now: Mutex<DateTime<Utc>>,
}

impl SessionClock {
    /// Starts at `at`.
    #[must_use]
    pub const fn new(at: DateTime<Utc>) -> Self {
        Self {
            now: Mutex::new(at),
        }
    }

    /// Moves the clock.
    pub fn set(&self, at: DateTime<Utc>) {
        *self
            .now
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = at;
    }
}

impl Clock for SessionClock {
    fn now(&self) -> DateTime<Utc> {
        *self
            .now
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// What a completed bar did to a simulated venue's working orders.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BarEvents {
    /// Fills, in order.
    pub fills: Vec<BrokerFill>,
    /// Day orders that expired unfilled.
    pub expired: Vec<OrderIntentId>,
}

/// A venue whose order events the session reads bar by bar: the simulated
/// venues of backtests and paper trading fill orders from the bar; a live
/// broker adapter reports what the broker did on that bar's date.
pub trait SimulatedVenue: BrokerAccountReader {
    /// Processes one completed bar for an instrument.
    fn process_bar(&self, instrument: InstrumentId, bar: &Bar) -> BarEvents;
    /// Checks protective stops placed on this bar against the rest of the bar.
    fn process_same_bar_protection(&self, instrument: InstrumentId, bar: &Bar) -> BarEvents;
}

/// One instrument's data for a session.
#[derive(Clone, Debug)]
pub struct InstrumentData {
    /// Spec.
    pub spec: InstrumentSpec,
    /// Product to trade.
    pub product: ProductType,
    /// All completed bars known to the run.
    pub series: BarSeries,
}

/// A strategy version the session evaluates.
#[derive(Clone)]
pub struct StrategySlot<'a> {
    /// The logic.
    pub strategy: &'a dyn Strategy,
    /// Identity, stage, RR floor and the default slippage assumption.
    pub info: StrategyVersionInfo,
    /// When set, the stop-slippage assumption is this many ticks of each
    /// instrument instead of `info.slippage`.
    pub slippage_ticks: Option<Decimal>,
}

impl std::fmt::Debug for StrategySlot<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StrategySlot")
            .field("info", &self.info)
            .field("slippage_ticks", &self.slippage_ticks)
            .finish_non_exhaustive()
    }
}

/// One closed trade.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TradeRecord {
    /// Position.
    pub position: PositionId,
    /// The decision that opened it (absent in entries written before Phase 7).
    #[serde(default)]
    pub decision: Option<DecisionId>,
    /// Setup type.
    #[serde(default)]
    pub setup_type: String,
    /// Symbol.
    pub instrument: String,
    /// Side.
    pub side: Side,
    /// Entry fill date.
    pub opened_on: NaiveDate,
    /// Date the trade closed.
    pub closed_on: NaiveDate,
    /// Quantity entered.
    pub quantity: Quantity,
    /// Average entry price.
    pub entry_price: Decimal,
    /// Average exit price.
    pub exit_price: Decimal,
    /// P&L before costs.
    pub gross_pnl: Decimal,
    /// Round-trip costs from the cost model.
    pub costs: Decimal,
    /// P&L after costs.
    pub net_pnl: Decimal,
    /// Net P&L over the risk planned at entry.
    pub r_multiple: Decimal,
    /// Why it closed.
    pub exit_reason: ExitReason,
}

/// The account's running book, in the account currency.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountBook {
    /// Account.
    pub id: AccountId,
    /// Currency.
    pub currency: Currency,
    /// Starting equity.
    pub initial: Decimal,
    /// Realized P&L after costs.
    pub realized_net: Decimal,
    /// Highest equity at a close.
    pub high_water_mark: Decimal,
    /// Equity at the start of the current day.
    pub day_start: Decimal,
    /// Equity at the start of the current ISO week.
    pub week_start: Decimal,
    /// Current ISO week (year, week).
    pub week: (i32, u32),
    /// Losing trades in a row.
    pub consecutive_losses: u32,
}

impl AccountBook {
    /// A fresh book.
    #[must_use]
    pub fn new(id: AccountId, initial: Money, first: NaiveDate) -> Self {
        Self {
            id,
            currency: initial.currency,
            initial: initial.amount,
            realized_net: Decimal::ZERO,
            high_water_mark: initial.amount,
            day_start: initial.amount,
            week_start: initial.amount,
            week: (first.iso_week().year(), first.iso_week().week()),
            consecutive_losses: 0,
        }
    }
}

/// One processed trading day, journaled as `day_closed`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DayRecord {
    /// Account.
    pub account: AccountId,
    /// Trading date.
    pub date: NaiveDate,
    /// Equity at the close, marked to market.
    pub equity: Decimal,
    /// The book after the day.
    pub book: AccountBook,
    /// Trades booked today.
    pub trades: Vec<TradeRecord>,
    /// Decisions by outcome code (`enter` headline or the NO TRADE reason code).
    pub decisions: BTreeMap<String, u32>,
    /// Positions left without protection today.
    pub unprotected: u32,
    /// Book/venue reconciliation mismatches at the close.
    pub mismatches: u32,
}

/// Why a session step failed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SessionError {
    /// The journal failed; the day is not complete.
    #[error("journal failure: {0}")]
    Journal(String),
    /// The date is not after the last processed date.
    #[error("{0} was already processed")]
    AlreadyProcessed(NaiveDate),
}

/// An entry the AI agent asks for, outside the daily cycle (ADR 0016).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentEntry {
    /// Instrument.
    pub instrument: InstrumentId,
    /// The agent's plan and explanation.
    pub candidate: SetupCandidate,
    /// The agent's probabilities and expected time-exit R.
    pub probabilities: qd_domain::economics::OutcomeProbabilities,
    /// Expected R at the time exit.
    pub time_exit_r: Decimal,
    /// Capital the agent allocates, account currency: entry notional ceiling.
    pub allocation: Decimal,
    /// The agent's identity, stage and RR floor.
    pub info: StrategyVersionInfo,
}

/// What an agent entry came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentEntryOutcome {
    /// The journaled decision (post-risk, INV-17).
    pub decision: JournaledDecision,
    /// The allocation as a quantity ceiling.
    pub requested_quantity: Quantity,
    /// The position opened, when the entry was approved and submitted.
    pub position: Option<PositionId>,
    /// Why the gateway refused an approved entry.
    pub rejection: Option<String>,
}

/// Why an agent request could not be decided.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AgentError {
    /// The instrument is not in the run.
    #[error("instrument {0} is not loaded")]
    UnknownInstrument(InstrumentId),
    /// Too little history for the feature set recorded with every decision.
    #[error("not enough history: {0}")]
    History(String),
    /// No such active position, or the agent did not open it.
    #[error("no active agent position {0}")]
    UnknownPosition(PositionId),
    /// The allocation is not positive.
    #[error("the allocation must be positive")]
    Allocation,
    /// The session failed.
    #[error(transparent)]
    Session(#[from] SessionError),
}

/// Settings of a session.
#[derive(Clone, Debug)]
pub struct SessionSettings {
    /// Account mode (backtest or paper; the session never runs live).
    pub mode: AccountMode,
    /// Time of day (UTC) at which each date's bar counts as complete.
    pub close_time_utc: NaiveTime,
    /// Trading-calendar version recorded with each decision.
    pub calendar_version: String,
    /// The data snapshot the run reads (INV-09).
    pub snapshot: SnapshotId,
}

/// Everything a session is built from.
pub struct SessionParts {
    /// The gateway (its executor is the venue).
    pub gateway: Arc<OrderGateway>,
    /// The venue.
    pub venue: Arc<dyn SimulatedVenue>,
    /// Journal.
    pub journal: Arc<dyn Journal>,
    /// Halts.
    pub halts: Arc<dyn HaltStore>,
    /// Market clock (each date's close).
    pub market_clock: Arc<SessionClock>,
    /// Wall clock (halts, decisions). The market clock in backtests.
    pub wall_clock: Arc<dyn Clock>,
    /// The book to start from.
    pub book: AccountBook,
    /// Settings.
    pub settings: SessionSettings,
}

/// A trading session over consecutive dates.
pub struct TradingSession {
    gateway: Arc<OrderGateway>,
    positions: PositionManager,
    venue: Arc<dyn SimulatedVenue>,
    journal: Arc<dyn Journal>,
    halts: Arc<dyn HaltStore>,
    market_clock: Arc<SessionClock>,
    wall_clock: Arc<dyn Clock>,
    book: AccountBook,
    recorded: BTreeSet<PositionId>,
    last_date: Option<NaiveDate>,
    closes: HashMap<InstrumentId, Price>,
    settings: SessionSettings,
}

impl std::fmt::Debug for TradingSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TradingSession")
            .field("book", &self.book)
            .field("last_date", &self.last_date)
            .finish_non_exhaustive()
    }
}

impl TradingSession {
    /// A new session with an empty book of positions.
    #[must_use]
    pub fn new(parts: SessionParts) -> Self {
        let positions = PositionManager::new(
            parts.gateway.clone(),
            parts.journal.clone(),
            parts.market_clock.clone(),
        );
        Self {
            gateway: parts.gateway,
            positions,
            venue: parts.venue,
            journal: parts.journal,
            halts: parts.halts,
            market_clock: parts.market_clock,
            wall_clock: parts.wall_clock,
            book: parts.book,
            recorded: BTreeSet::new(),
            last_date: None,
            closes: HashMap::new(),
            settings: parts.settings,
        }
    }

    /// A session continuing from restored journal state. The caller must
    /// have checked the state ([`RestoredState::check`]) and restored the
    /// venue from it.
    #[must_use]
    pub fn restored(parts: SessionParts, restored: &RestoredState) -> Self {
        parts.gateway.restore(restored);
        let mut session = Self::new(parts);
        session.positions.restore(restored);
        session.recorded.clone_from(&restored.recorded);
        if let Some(day) = &restored.last_day {
            session.book = day.book.clone();
            session.last_date = Some(day.date);
        }
        session
    }

    /// Resolves intents whose outcome was unknown at restore time and that
    /// the simulated venue does not hold: they never reached it. Entries are
    /// abandoned; the Position Manager sees the rest as not placed.
    pub async fn reconcile_unknown_with_venue(&self) -> Vec<String> {
        let mut problems = Vec::new();
        for intent in self.gateway.unknown_intents() {
            match self
                .gateway
                .reconcile_intent(intent.id(), ReconciledState::BrokerRejected)
                .await
            {
                Ok(_) => self.positions.on_entry_ended(intent.id()).await,
                Err(e) => problems.push(e.to_string()),
            }
        }
        problems
    }

    /// The Position Manager.
    #[must_use]
    pub const fn positions(&self) -> &PositionManager {
        &self.positions
    }

    /// The Order Gateway.
    #[must_use]
    pub const fn gateway(&self) -> &Arc<OrderGateway> {
        &self.gateway
    }

    /// The account book.
    #[must_use]
    pub const fn book(&self) -> &AccountBook {
        &self.book
    }

    /// The last processed date.
    #[must_use]
    pub const fn last_date(&self) -> Option<NaiveDate> {
        self.last_date
    }

    /// The close time of a date.
    #[must_use]
    pub fn close_of(&self, date: NaiveDate) -> DateTime<Utc> {
        Utc.from_utc_datetime(&date.and_time(self.settings.close_time_utc))
    }

    /// Processes one trading date. Dates must be processed in order.
    #[allow(clippy::too_many_lines)] // one linear daily sequence reads best in one place
    pub async fn process_day(
        &mut self,
        date: NaiveDate,
        instruments: &[InstrumentData],
        slots: &[StrategySlot<'_>],
        engine: &DecisionEngine<'_>,
        risk: &RiskConfig,
        costs: &dyn CostModel,
    ) -> Result<DayRecord, SessionError> {
        if self.last_date.is_some_and(|last| date <= last) {
            return Err(SessionError::AlreadyProcessed(date));
        }
        self.market_clock.set(self.close_of(date));
        let by_id: HashMap<InstrumentId, &InstrumentData> =
            instruments.iter().map(|d| (d.spec.id, d)).collect();
        let mut day = DayRecord {
            account: self.book.id,
            date,
            equity: Decimal::ZERO,
            book: self.book.clone(),
            trades: Vec::new(),
            decisions: BTreeMap::new(),
            unprotected: 0,
            mismatches: 0,
        };

        // 1–3. Bars, fills, exits.
        for data in instruments {
            let Some(bar) = data
                .series
                .bars()
                .iter()
                .find(|b| b.date() == date)
                .copied()
            else {
                continue;
            };
            self.closes.insert(data.spec.id, bar.close());
            let events = self.venue.process_bar(data.spec.id, &bar);
            self.apply_events(events, date, &mut day).await;
            let same_bar = self.venue.process_same_bar_protection(data.spec.id, &bar);
            self.apply_events(same_bar, date, &mut day).await;
            let update = self.positions.on_bar_close(data.spec.id, &bar).await;
            count_update(&update, &mut day);
        }

        // Book closed trades, mark to market.
        for p in self.positions.positions() {
            if p.state == PositionState::Closed
                && !self.recorded.contains(&p.id)
                && !p.entered_quantity.is_zero()
            {
                self.recorded.insert(p.id);
                if let Some(data) = by_id.get(&p.instrument) {
                    let trade = close_trade(&p, data, costs, date);
                    self.book.realized_net += trade.net_pnl;
                    self.book.consecutive_losses = if trade.net_pnl < Decimal::ZERO {
                        self.book.consecutive_losses + 1
                    } else {
                        0
                    };
                    day.trades.push(trade);
                }
            }
        }
        let unrealized: Decimal = self
            .positions
            .active()
            .iter()
            .map(|p| unrealized(p, self.closes.get(&p.instrument).copied()))
            .sum();
        let equity = self.book.initial + self.book.realized_net + unrealized;
        day.equity = equity;
        let week = (date.iso_week().year(), date.iso_week().week());
        if week != self.book.week {
            self.book.week = week;
            self.book.week_start = equity;
        }

        // Halt triggers and reconciliation.
        let state = self.risk_state(equity, &by_id, risk).await;
        let now = self.wall_clock.now();
        if let Ok(proposals) = halt_triggers(&state, risk, now) {
            for proposal in proposals {
                if let Ok(halt) = proposal.into_halt(HaltId::new_at(now), now) {
                    // A failed halt write is not silent: the next risk check
                    // reads the store again, and an unreadable store halts entries.
                    let _ = self.halts.record(&halt).await;
                }
            }
        }
        let reconciliation = match self.venue.positions().await {
            Ok(venue_positions) => {
                let mismatches = self.positions.reconcile(&venue_positions).len();
                day.mismatches = u32::try_from(mismatches).unwrap_or(u32::MAX);
                (mismatches > 0)
                    .then(|| format!("{mismatches} position mismatch(es) with the broker"))
            }
            Err(e) => Some(format!("broker positions unreadable: {e}")),
        };
        // A live book that disagrees with the broker, or cannot be checked,
        // must not add risk (INV-06): halt entries until a human looks.
        if let (AccountMode::Live, Some(problem)) = (self.settings.mode, reconciliation) {
            if let Ok(halt) = qd_domain::halt::Halt::new(
                HaltId::new_at(now),
                qd_domain::halt::HaltKind::Operational,
                qd_domain::halt::HaltScope::Account(self.book.id),
                format!("live reconciliation failed on {date}: {problem}"),
                now,
                None,
                true,
            ) {
                let _ = self.halts.record(&halt).await;
            }
        }

        // Decisions at the close, for instruments without an active position.
        let state = self.risk_state(equity, &by_id, risk).await;
        for data in instruments {
            if !data.series.bars().iter().any(|b| b.date() == date) {
                continue;
            }
            let series = data.series.as_of(date);
            for slot in slots {
                if self.positions.has_active(data.spec.id) {
                    break;
                }
                self.decide(date, data, &series, slot, &state, engine, &mut day)
                    .await?;
            }
        }

        self.book.day_start = equity;
        self.book.high_water_mark = self.book.high_water_mark.max(equity);
        day.book = self.book.clone();
        self.journal
            .append(&JournalEntry::DayClosed(Box::new(day.clone())))
            .await
            .map_err(|e| SessionError::Journal(e.to_string()))?;
        self.last_date = Some(date);
        Ok(day)
    }

    #[allow(clippy::too_many_arguments)] // one decision needs each of these inputs
    async fn decide(
        &self,
        date: NaiveDate,
        data: &InstrumentData,
        series: &BarSeries,
        slot: &StrategySlot<'_>,
        state: &AccountRiskState,
        engine: &DecisionEngine<'_>,
        day: &mut DayRecord,
    ) -> Result<(), SessionError> {
        let Ok(evaluation) = run_strategy(slot.strategy, &data.spec, series, &RegimeClassifier::V1)
        else {
            return Ok(());
        };
        let mut info = slot.info.clone();
        if let Some(ticks) = slot.slippage_ticks {
            if let Ok(s) = qd_domain::economics::SlippageAssumption::new(
                info.slippage.model_version().to_owned(),
                ticks * data.spec.tick_size,
            ) {
                info.slippage = s;
            }
        }
        let now = self.wall_clock.now();
        let ctx = DecisionContext {
            decision_id: DecisionId::new_at(now),
            at: now,
            expected_last_completed: date,
            spec: &data.spec,
            product: data.product,
            fx: FxRate::identity(self.book.currency, now),
            strategy: &info,
            evaluation: &evaluation,
            series,
            account: state,
            already_in_position: self.positions.has_active(data.spec.id),
            snapshot_id: self.settings.snapshot,
            calendar_version: &self.settings.calendar_version,
        };
        let decided = engine
            .decide_and_journal(&ctx, self.journal.as_ref())
            .await
            .map_err(|e| SessionError::Journal(e.to_string()))?;
        let Some(decided) = decided else {
            return Ok(());
        };
        let code = match &decided.record.outcome {
            DecisionOutcome::NoTrade { reason } => reason.code().to_owned(),
            other => other.headline().to_owned(),
        };
        *day.decisions.entry(code).or_default() += 1;
        if let (Some(authorization), Some(proposal)) =
            (decided.authorization, decided.record.proposal.as_ref())
        {
            // A refused entry is already journaled by the gateway.
            let _: Result<PositionId, GatewayRejection> = self
                .positions
                .open(
                    &authorization,
                    proposal,
                    &data.spec,
                    info.stage,
                    data.product,
                )
                .await;
        }
        Ok(())
    }

    async fn apply_events(&self, events: BarEvents, date: NaiveDate, day: &mut DayRecord) {
        for fill in events.fills {
            if let Ok(report) = self.gateway.on_fill(fill).await {
                let update = self.positions.on_fill(&report, date).await;
                count_update(&update, day);
            }
        }
        for expired in events.expired {
            if self.gateway.on_expired(expired).await.is_ok() {
                self.positions.on_entry_ended(expired).await;
                if self.positions.on_protection_ended(expired).await.is_some() {
                    day.unprotected = day.unprotected.saturating_add(1);
                }
            }
        }
    }

    /// Applies venue events outside the daily cycle (a live broker's fills
    /// during the day, so protection is placed as soon as an entry fills).
    /// Returns the day record fragment with the counts.
    pub async fn apply_events_now(&self, events: BarEvents, date: NaiveDate) -> DayRecord {
        let mut day = DayRecord {
            account: self.book.id,
            date,
            equity: Decimal::ZERO,
            book: self.book.clone(),
            trades: Vec::new(),
            decisions: BTreeMap::new(),
            unprotected: 0,
            mismatches: 0,
        };
        self.apply_events(events, date, &mut day).await;
        day
    }

    /// Marks every instrument at its last close on or before `date`.
    fn mark_to(&mut self, date: NaiveDate, instruments: &[InstrumentData]) {
        for data in instruments {
            if let Some(bar) = data.series.bars().iter().rev().find(|b| b.date() <= date) {
                self.closes.insert(data.spec.id, bar.close());
            }
        }
    }

    /// Equity marked at the last closes on or before `date`, and the
    /// account state the Risk Gate would see now.
    pub async fn risk_state_at(
        &mut self,
        date: NaiveDate,
        instruments: &[InstrumentData],
        risk: &RiskConfig,
    ) -> (Decimal, AccountRiskState) {
        self.mark_to(date, instruments);
        let unrealized: Decimal = self
            .positions
            .active()
            .iter()
            .map(|p| unrealized(p, self.closes.get(&p.instrument).copied()))
            .sum();
        let equity = self.book.initial + self.book.realized_net + unrealized;
        let by_id: HashMap<InstrumentId, &InstrumentData> =
            instruments.iter().map(|d| (d.spec.id, d)).collect();
        let state = self.risk_state(equity, &by_id, risk).await;
        (equity, state)
    }

    /// An AI agent's entry, decided after the close of `date` (the last
    /// completed bar) through the same Decision Engine, Risk Gate, Position
    /// Manager and Order Gateway as every strategy (INV-01, INV-03, INV-08).
    /// The allocation only caps the size; the Risk Gate decides it.
    pub async fn agent_entry(
        &mut self,
        date: NaiveDate,
        instruments: &[InstrumentData],
        request: &AgentEntry,
        engine: &DecisionEngine<'_>,
        risk: &RiskConfig,
    ) -> Result<AgentEntryOutcome, AgentError> {
        if request.allocation <= Decimal::ZERO {
            return Err(AgentError::Allocation);
        }
        let data = instruments
            .iter()
            .find(|d| d.spec.id == request.instrument)
            .ok_or(AgentError::UnknownInstrument(request.instrument))?;
        let series = data.series.as_of(date);
        let features =
            FeatureSet::compute(&series).map_err(|e| AgentError::History(e.to_string()))?;
        let regime = RegimeClassifier::V1.classify(&features);
        let evaluation = Evaluation {
            features,
            regime,
            output: StrategyOutput::Setup(Box::new(request.candidate.clone())),
        };
        self.market_clock.set(self.close_of(date));
        let (_, state) = self.risk_state_at(date, instruments, risk).await;
        let now = self.wall_clock.now();
        // Allocation → quantity ceiling at the planned entry, rounded down.
        let per_unit = request.candidate.plan.entry * data.spec.multiplier;
        let requested_quantity = request
            .allocation
            .checked_div(per_unit)
            .and_then(|raw| data.spec.round_quantity_down(raw).ok())
            .unwrap_or(Quantity::ZERO);
        let forecast = AgentForecast {
            probabilities: request.probabilities.clone(),
            time_exit_r: request.time_exit_r,
            max_quantity: requested_quantity,
        };
        let ctx = DecisionContext {
            decision_id: DecisionId::new_at(now),
            at: now,
            expected_last_completed: date,
            spec: &data.spec,
            product: data.product,
            fx: FxRate::identity(self.book.currency, now),
            strategy: &request.info,
            evaluation: &evaluation,
            series: &series,
            account: &state,
            already_in_position: self.positions.has_active(data.spec.id),
            snapshot_id: self.settings.snapshot,
            calendar_version: &self.settings.calendar_version,
        };
        let decision = engine
            .decide_agent_and_journal(&ctx, &forecast, self.journal.as_ref())
            .await
            .map_err(|e| SessionError::Journal(e.to_string()))?
            .ok_or_else(|| SessionError::Journal("the agent setup was not decided".to_owned()))?;
        let mut position = None;
        let mut rejection = None;
        if let (Some(authorization), Some(proposal)) =
            (&decision.authorization, decision.record.proposal.as_ref())
        {
            match self
                .positions
                .open(
                    authorization,
                    proposal,
                    &data.spec,
                    request.info.stage,
                    data.product,
                )
                .await
            {
                Ok(id) => position = Some(id),
                Err(e) => rejection = Some(e.to_string()),
            }
        }
        Ok(AgentEntryOutcome {
            decision,
            requested_quantity,
            position,
            rejection,
        })
    }

    /// Closes an active position the agent opened (never blocked by halts,
    /// INV-02). Positions of other strategies are refused.
    pub async fn agent_exit(
        &self,
        position: PositionId,
        agent: qd_domain::ids::StrategyVersionId,
    ) -> Result<PositionUpdate, AgentError> {
        let Some(p) = self
            .positions
            .active()
            .into_iter()
            .find(|p| p.id == position && p.strategy_version == Some(agent))
        else {
            return Err(AgentError::UnknownPosition(position));
        };
        let mut update = PositionUpdate::default();
        // An entry that has not filled yet is cancelled instead.
        for working in self.gateway.working_intents(position) {
            if working.risk_effect() == qd_domain::action::RiskEffect::Increasing {
                if let Err(e) = self.gateway.cancel(working.id()).await {
                    update.rejections.push(e.to_string());
                    continue;
                }
                let still_working = self
                    .gateway
                    .working_intents(position)
                    .iter()
                    .any(|w| w.id() == working.id());
                if still_working {
                    update
                        .rejections
                        .push("the entry cancel is not confirmed yet".to_owned());
                } else if p.quantity.is_zero() {
                    self.positions.on_entry_ended(working.id()).await;
                }
            }
        }
        self.positions
            .exit(position, ExitReason::Agent, &mut update)
            .await;
        Ok(update)
    }

    async fn risk_state(
        &self,
        equity: Decimal,
        by_id: &HashMap<InstrumentId, &InstrumentData>,
        risk: &RiskConfig,
    ) -> AccountRiskState {
        let book = &self.book;
        let money = |amount: Decimal| Money::new(amount, book.currency);
        let at = self.wall_clock.now();
        let mut open_risk = Vec::new();
        for p in self.positions.active() {
            let Some(data) = by_id.get(&p.instrument) else {
                // An open position without data cannot be valued: count its
                // planned risk rather than nothing (fail closed, INV-06).
                open_risk.push(RiskItem {
                    instrument: p.instrument,
                    strategy_version: p.strategy_version,
                    bucket: qd_domain::instrument::CorrelationBucket("unknown".to_owned()),
                    amount: money(p.risk_net_per_unit * p.planned_quantity.value()),
                });
                continue;
            };
            let fx = FxRate::identity(book.currency, at);
            let amount = if p.state == PositionState::Opening {
                money(p.risk_net_per_unit * p.planned_quantity.value())
            } else {
                let Some(mark) = self.closes.get(&p.instrument).copied() else {
                    continue;
                };
                let protection = if p.state == PositionState::Protected {
                    Protection::Valid { stop: p.stop }
                } else {
                    Protection::Missing
                };
                match position_open_risk(&PositionExposure {
                    side: p.side,
                    quantity: p.quantity,
                    mark,
                    protection,
                    multiplier: p.multiplier,
                    fx,
                    gap_shock: risk.gap_shocks.for_asset_class(data.spec.asset_class),
                }) {
                    Ok(r) => r.amount,
                    Err(_) => continue,
                }
            };
            open_risk.push(RiskItem {
                instrument: p.instrument,
                strategy_version: p.strategy_version,
                bucket: data.spec.correlation_bucket.clone(),
                amount,
            });
        }
        let halt_state = match self.halts.load().await {
            Ok(h) => HaltState::Known(h),
            Err(_) => HaltState::Unknown,
        };
        AccountRiskState {
            account: book.id,
            mode: self.settings.mode,
            equity: money(equity),
            equity_at_day_start: money(book.day_start),
            equity_at_week_start: money(book.week_start),
            high_water_mark: money(book.high_water_mark.max(equity)),
            consecutive_losses: book.consecutive_losses,
            open_risk,
            halts: halt_state,
        }
    }
}

fn count_update(update: &PositionUpdate, day: &mut DayRecord) {
    day.unprotected += u32::try_from(update.unprotected.len()).unwrap_or(u32::MAX);
}

/// Unrealized P&L of a position at a mark, instrument currency.
#[must_use]
pub fn unrealized(p: &Position, mark: Option<Price>) -> Decimal {
    let (Some(entry), Some(mark)) = (p.entry_price, mark) else {
        return Decimal::ZERO;
    };
    let per_unit = match p.side {
        Side::Long => mark.value() - entry.value(),
        Side::Short => entry.value() - mark.value(),
    };
    per_unit * p.quantity.value() * p.multiplier
}

fn close_trade(
    p: &Position,
    data: &InstrumentData,
    costs: &dyn CostModel,
    date: NaiveDate,
) -> TradeRecord {
    let entry = p.entry_price.map_or(Decimal::ZERO, Price::value);
    let units = p.entered_quantity.value() * p.multiplier;
    let exit_per_unit = if units.is_zero() {
        Decimal::ZERO
    } else {
        p.realized_gross / units
    };
    let exit = match p.side {
        Side::Long => entry + exit_per_unit,
        Side::Short => entry - exit_per_unit,
    };
    let cost = match (Price::new(entry), Price::new(exit)) {
        (Ok(e), Ok(x)) => costs
            .quote(&CostRequest {
                spec: &data.spec,
                product: data.product,
                side: p.side,
                quantity: p.entered_quantity,
                entry: e,
                exit: x,
                trade_date: date,
            })
            .map(|q| q.estimate.total())
            .unwrap_or(Decimal::ZERO),
        _ => Decimal::ZERO,
    };
    let net = p.realized_gross - cost;
    let planned = p.risk_net_per_unit * p.entered_quantity.value();
    let r = if planned.is_zero() {
        Decimal::ZERO
    } else {
        net / planned
    };
    TradeRecord {
        position: p.id,
        decision: Some(p.decision),
        setup_type: p.setup_type.clone(),
        instrument: data.spec.symbol.clone(),
        side: p.side,
        opened_on: p.opened_on.unwrap_or(date),
        closed_on: date,
        quantity: p.entered_quantity,
        entry_price: entry,
        exit_price: exit,
        gross_pnl: p.realized_gross,
        costs: cost,
        net_pnl: net,
        r_multiple: r,
        exit_reason: p.exit_reason.unwrap_or(ExitReason::Manual),
    }
}
