//! QuantDesk backtesting (spec §5.3 `qd-backtest`).
//!
//! A simulated clock and broker plugged into the same Decision Engine, Risk
//! Gate, Order Gateway and Position Manager that paper and live use (INV-08).
//! No logic is duplicated here: only time, data and fills are simulated.
//!
//! - [`sim`]: `SimClock` and `SimBroker` (conservative daily-bar fill rules).
//! - [`engine`]: the backtest loop, `run_backtest`.
//! - [`metrics`]: trade records and summary statistics.

pub mod engine;
pub mod metrics;
pub mod sim;
