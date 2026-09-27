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

/// What an AI advisor thinks of a decision. Advisory only (INV-04): nothing
/// that places orders, sizes positions, sets limits or halts ever reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiStance {
    /// Sees no problem.
    Agree,
    /// Sees risks worth a look.
    Caution,
    /// Would not take the trade.
    Disagree,
    /// Has no view (for example, not enough information).
    Abstain,
}

/// One advisor's advice on one decision, journaled in shadow mode.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct AiAdvice {
    /// Id.
    pub id: qd_domain::ids::AiReviewId,
    /// The decision advised on.
    pub decision: DecisionId,
    /// Advisor name and version, e.g. `checklist-v1`.
    pub advisor: String,
    /// Stance.
    pub stance: AiStance,
    /// Confidence, 0–1.
    pub confidence: rust_decimal::Decimal,
    /// Plain-text summary (bounded length, no markup).
    pub summary: String,
    /// Specific concerns.
    pub flags: Vec<String>,
    /// When.
    pub at: DateTime<Utc>,
}

/// Which way an AI prediction expects the price to move.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PredictedDirection {
    /// Higher at the horizon.
    Up,
    /// Lower at the horizon.
    Down,
}

/// One AI prediction (ADR 0016), journaled when it is made and scored
/// against realized prices after its horizon.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct AiPrediction {
    /// Id.
    pub id: qd_domain::ids::PredictionId,
    /// The agent run that made it.
    pub run: qd_domain::ids::AgentRunId,
    /// Model, e.g. `openai:gpt-6-astra`.
    pub model: String,
    /// Instrument.
    pub instrument: InstrumentId,
    /// Symbol, for display.
    pub symbol: String,
    /// Expected direction.
    pub direction: PredictedDirection,
    /// Horizon in trading days (completed daily bars).
    pub horizon_days: u16,
    /// Last completed close when the prediction was made.
    pub reference_price: rust_decimal::Decimal,
    /// Last completed bar date when the prediction was made.
    pub reference_date: NaiveDate,
    /// Price the move should reach, if stated.
    pub target_price: Option<rust_decimal::Decimal>,
    /// Price that proves it wrong, if stated.
    pub stop_price: Option<rust_decimal::Decimal>,
    /// The model's probability that it comes true (0–1).
    pub probability: rust_decimal::Decimal,
    /// The thesis in the model's words (bounded length).
    pub thesis: String,
    /// Sources the model cited.
    pub sources: Vec<String>,
    /// The decision it traded under, if it led to a trade.
    pub decision: Option<DecisionId>,
    /// Book of that trade (`paper` or `live`).
    pub book: Option<String>,
    /// When.
    pub at: DateTime<Utc>,
}

/// How an AI prediction turned out (ADR 0016).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct AiPredictionOutcome {
    /// The prediction.
    pub prediction: qd_domain::ids::PredictionId,
    /// Bar date of the horizon (or of the first target/stop touch).
    pub as_of: NaiveDate,
    /// Close at the horizon bar.
    pub end_price: rust_decimal::Decimal,
    /// `end_price / reference_price − 1`.
    pub return_pct: rust_decimal::Decimal,
    /// Whether the price moved the predicted way by the horizon.
    pub direction_correct: bool,
    /// `target`, `stop` or `neither`, when levels were stated.
    pub levels: Option<String>,
    /// Whether the prediction counts as correct (target first when stated,
    /// else the direction).
    pub correct: bool,
    /// When it was scored.
    pub at: DateTime<Utc>,
}

/// One tool call in an agent run's trace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct AgentStep {
    /// Tool name.
    pub tool: String,
    /// Arguments as sent by the model (bounded).
    pub arguments: serde_json::Value,
    /// Result as returned to the model (bounded).
    pub result: serde_json::Value,
    /// Duration in milliseconds.
    pub millis: u64,
}

/// One agent run: what the AI looked at, did and concluded (ADR 0016).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct AgentRunRecord {
    /// Id.
    pub id: qd_domain::ids::AgentRunId,
    /// `research`, `monitor` or `manual` (not `kind`: that is the entry tag).
    pub run_kind: String,
    /// Model.
    pub model: String,
    /// Book traded (`paper` or `live`).
    pub book: String,
    /// Start.
    pub started_at: DateTime<Utc>,
    /// End.
    pub finished_at: DateTime<Utc>,
    /// `completed`, `budget_exhausted`, `timeout` or `failed`.
    pub status: String,
    /// The model's closing summary.
    pub summary: String,
    /// Tool calls in order.
    pub steps: Vec<AgentStep>,
    /// Decisions the run's trade requests produced.
    pub decisions: Vec<DecisionId>,
    /// Predictions it recorded.
    pub predictions: Vec<qd_domain::ids::PredictionId>,
    /// What went wrong, if anything.
    pub error: Option<String>,
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
    /// Advisory AI output about a decision (INV-04, shadow mode).
    AiAdvice(Box<AiAdvice>),
    /// An AI agent run and its tool trace (ADR 0016).
    AgentRun(Box<AgentRunRecord>),
    /// An AI prediction (ADR 0016).
    AiPrediction(Box<AiPrediction>),
    /// How an AI prediction turned out (ADR 0016).
    AiPredictionOutcome(Box<AiPredictionOutcome>),
    /// A halt was recorded or cleared.
    Halt {
        /// The halt.
        halt: Box<Halt>,
    },
}
