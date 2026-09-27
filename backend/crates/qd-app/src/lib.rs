//! QuantDesk application layer: use cases and the ports they need (spec §5.3 `qd-app`).
//!
//! - [`ports`]: clock, journal, halt store, broker order executor and account
//!   reader, evidence source.
//! - [`orders`]: order intents, entry authorizations, broker order requests.
//! - [`decision`]: Trade Proposal Engine and Decision Engine.
//! - [`gateway`]: the Order Gateway, the only order path.
//! - [`positions`]: the Position Manager.
//! - [`journal`]: Decision Journal entries.
//! - [`live`]: the live-trading gate (INV-14).
//! - [`registry`]: the Strategy Registry (immutable versions, stage history).
//! - [`monitor`]: health, alerts and Prometheus metrics.
//! - [`memory`]: in-memory journal and halt store for backtests and tests.
//! - [`evidence`]: recorded evidence (INV-11) and evidence tables.
//! - [`review`]: predicted-vs-realized review, calibration and paper-review evidence.
//! - [`secrets`]: the catalog of secrets the owner can enter in the web UI.
//! - [`session`]: the daily trading cycle shared by backtest and paper (INV-08).
//! - [`restore`]: rebuilding trading state from the journal after a restart.
//! - [`book_view`]: equity curve, drawdown, exposure and P&L per strategy.
//! - [`runs`]: loading state, strategy slots and instruments for a run.

pub mod book_view;
pub mod decision;
pub mod evidence;
pub mod gateway;
pub mod journal;
pub mod live;
pub mod memory;
pub mod monitor;
pub mod orders;
pub mod ports;
pub mod positions;
pub mod registry;
pub mod restore;
pub mod review;
pub mod runs;
pub mod secrets;
pub mod session;
