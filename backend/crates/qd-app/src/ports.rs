//! Ports: every external dependency of the use cases (spec §5.4).
//!
//! Adapters (store, brokers, market data) implement these; only the binaries
//! wire them together. The order-placing capability is a separate trait,
//! [`BrokerOrderExecutor`], handed only to the Order Gateway (INV-01).

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use qd_domain::economics::OutcomeProbabilities;
use qd_domain::halt::Halt;
use qd_domain::ids::{InstrumentId, OrderIntentId, StrategyVersionId};
use qd_domain::num::{Price, Quantity};
use rust_decimal::Decimal;
use serde::Serialize;
use thiserror::Error;

use crate::journal::JournalEntry;
use crate::orders::BrokerOrderRequest;

/// Source of the current time. `SimClock` in backtests, the system clock in the server.
pub trait Clock: Send + Sync {
    /// Now.
    fn now(&self) -> DateTime<Utc>;
}

/// A durable journal write failed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("journal write failed: {0}")]
pub struct JournalError(pub String);

/// The append-only Decision Journal (INV-05, INV-16).
#[async_trait]
pub trait Journal: Send + Sync {
    /// Durably appends an entry and returns its sequence number. Returns only
    /// after the entry is durable.
    async fn append(&self, entry: &JournalEntry) -> Result<u64, JournalError>;
}

/// A storage operation failed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("store error: {0}")]
pub struct StoreError(pub String);

/// Durable kill-switch state (INV-07: hard halts survive restarts).
#[async_trait]
pub trait HaltStore: Send + Sync {
    /// Every halt ever recorded, latest version of each.
    async fn load(&self) -> Result<Vec<Halt>, StoreError>;
    /// Records a new halt or a cleared version of an existing one.
    async fn record(&self, halt: &Halt) -> Result<(), StoreError>;
}

/// Broker order id.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
pub struct BrokerOrderId(pub String);

/// A broker's acknowledgement of an order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BrokerOrderAck {
    /// The broker's id for the order.
    pub broker_order_id: BrokerOrderId,
}

/// A broker call failed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BrokerError {
    /// The broker refused the order; it does not exist at the broker.
    #[error("broker rejected the order: {0}")]
    Rejected(String),
    /// Timeout or transport failure: the outcome is unknown.
    #[error("broker transport error: {0}")]
    Transport(String),
}

/// Order-placing capability. Constructed only in the binaries and handed only
/// to the Order Gateway (INV-01).
#[async_trait]
pub trait BrokerOrderExecutor: Send + Sync {
    /// Places an order. Must be idempotent on `client_order_id`.
    async fn submit(&self, request: &BrokerOrderRequest) -> Result<BrokerOrderAck, BrokerError>;
    /// Cancels an order.
    async fn cancel(&self, client_order_id: OrderIntentId) -> Result<(), BrokerError>;
}

/// A net position as the broker reports it, for reconciliation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BrokerPosition {
    /// Instrument.
    pub instrument: InstrumentId,
    /// Signed net quantity: positive long, negative short. Broker-facing only.
    pub net_quantity: Decimal,
}

/// Read-only broker access.
#[async_trait]
pub trait BrokerAccountReader: Send + Sync {
    /// Net positions.
    async fn positions(&self) -> Result<Vec<BrokerPosition>, BrokerError>;
}

/// A fill reported by a broker.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct BrokerFill {
    /// Client order id.
    pub client_order_id: OrderIntentId,
    /// Filled quantity.
    pub quantity: Quantity,
    /// Fill price.
    pub price: Price,
    /// Fill time.
    pub at: DateTime<Utc>,
}

/// Outcome probabilities for a strategy version's setup, from validation evidence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Evidence {
    /// Probabilities for target first, stop first, time exit.
    pub probabilities: OutcomeProbabilities,
    /// `E[net P&L | time exit]` in R; converted per unit by the proposal engine.
    pub time_exit_r: Decimal,
}

/// Evidence tables produced by validation (Phase 7).
pub trait EvidenceSource: Send + Sync {
    /// Evidence for a version's setup type, if any exists.
    fn evidence(&self, version: StrategyVersionId, setup_type: &str) -> Option<Evidence>;
}

/// Historical market data: instrument specs and point-in-time daily bars.
#[async_trait]
pub trait HistoricalMarketData: Send + Sync {
    /// Every instrument spec version effective on `date`.
    async fn instruments(
        &self,
        date: chrono::NaiveDate,
    ) -> Result<Vec<qd_domain::instrument::InstrumentSpec>, StoreError>;
    /// Daily bars in `[from, to]` as known at `known_at`: corrections ingested
    /// later are invisible (INV-09).
    async fn daily_bars(
        &self,
        instrument: InstrumentId,
        from: chrono::NaiveDate,
        to: chrono::NaiveDate,
        known_at: DateTime<Utc>,
    ) -> Result<Vec<qd_domain::market::Bar>, StoreError>;
}

/// The append-only audit log of human and system actions.
#[async_trait]
pub trait AuditLog: Send + Sync {
    /// Appends one record.
    async fn record(
        &self,
        actor: &str,
        action: &str,
        detail: serde_json::Value,
    ) -> Result<(), StoreError>;
}
