//! QuantDesk paper trading (spec §5.3 `qd-broker-paper`).
//!
//! - [`venue`]: `PaperBroker`, the simulated venue with conservative
//!   daily-bar fill rules. Backtests use the same venue (INV-08).
//! - [`runner`]: `PaperRunner`, which runs the shared daily cycle over the
//!   stored data with state restored from the journal.
//!
//! Nothing in this crate can reach a real broker.

pub mod runner;
pub mod venue;

pub use runner::{NoEvidenceTables, PaperDeps, PaperRunner, PaperSettings};
pub use venue::{PaperBroker, simulate_fill};
