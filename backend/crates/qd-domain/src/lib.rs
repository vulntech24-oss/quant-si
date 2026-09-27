//! QuantDesk domain core.
//!
//! Pure types, formulas, rounding rules and state machines (spec §6). This crate
//! performs no I/O, never reads the clock (time is always passed in, spec §5.4)
//! and never uses binary floating point for prices, quantities or money
//! (INV-13). Backtest, paper and live all use this same code (INV-08).
//!
//! Module map:
//! - [`ids`]: typed UUIDv7 identifiers.
//! - [`num`]: `Price`, `Quantity`, `Money`, `Currency`, `Ratio`, `Bps`, `FxRate`.
//! - [`instrument`]: `InstrumentSpec` (data, never hardcoded) and tick/lot rounding.
//! - [`action`]: `TradeAction` and the explicit UI labels (INV-12).
//! - [`outcome`]: `DecisionOutcome`, `NoTradeReason`, `ExitReason`.
//! - [`plan`]: tick-rounded, validated trade plans.
//! - [`economics`]: per-unit economics, outcome probabilities, expected value.
//! - [`sizing`]: position sizing steps that do not depend on portfolio state.
//! - [`portfolio`]: open risk, daily P&L, drawdown, R-multiple.
//! - [`proposal`]: the complete `TradeProposal`.
//! - [`halt`]: halts (kill switch) and the entry/order checks they imply.
//! - [`order_rules`]: pure order-validation rules shared by every mode.
//! - [`lifecycle`]: strategy, order-intent and position state machines.

#![deny(
    clippy::float_arithmetic,
    clippy::float_cmp,
    clippy::print_stdout,
    clippy::print_stderr
)]

pub mod action;
pub mod economics;
pub mod halt;
pub mod ids;
pub mod instrument;
pub mod lifecycle;
pub mod num;
pub mod order_rules;
pub mod outcome;
pub mod plan;
pub mod portfolio;
pub mod proposal;
pub mod sizing;
