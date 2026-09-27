//! QuantDesk Risk Gate (spec §5.2, INV-03).
//!
//! Decides whether an entry may be taken and at what size. It can reject any
//! entry; nothing upstream can bypass it, manual entries included. It never
//! depends on AI output (INV-04), never reads the clock and performs no I/O,
//! so backtest, paper and live run the same rules (INV-08).
//!
//! - [`config`]: validated risk configuration (§4 defaults live in `backend/config/risk.toml`).
//! - [`gate`]: the entry evaluation.
//! - [`triggers`]: halts the account state calls for (hard halt, cool-off).

#![deny(
    clippy::float_arithmetic,
    clippy::float_cmp,
    clippy::print_stdout,
    clippy::print_stderr
)]

pub mod config;
pub mod gate;
pub mod triggers;

pub use config::{RiskConfig, RiskConfigData};
pub use gate::{AccountRiskState, ApprovedEntry, EntryRequest, RiskGate, RiskItem, RiskVerdict};
pub use triggers::{HaltProposal, halt_triggers};
