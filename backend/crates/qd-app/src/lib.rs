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
//! - [`memory`]: in-memory journal and halt store for backtests and tests.

pub mod decision;
pub mod gateway;
pub mod journal;
pub mod live;
pub mod memory;
pub mod orders;
pub mod ports;
pub mod positions;
