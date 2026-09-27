//! QuantDesk strategies (spec §5.2 "Foundation" features and regime, §5.3 `qd-strategy`).
//!
//! Pure and deterministic: no I/O, no clock, no floats. Strategies see only a
//! [`BarSeries`](qd_domain::market::BarSeries) of completed bars (INV-09), and
//! the same code runs in backtest, paper and live (INV-08). A strategy
//! proposes a plan; it never sizes a position and never sees AI output.
//!
//! - [`catalog`]: every implementation in the build, for registry matching.
//! - [`features`]: versioned feature set computed from completed daily bars.
//! - [`regime`]: deterministic regime classifier.
//! - [`strategy`]: the `Strategy` trait, its output and [`strategy::run_strategy`].
//! - [`trend_pullback`]: the first strategy, a long pullback in an uptrend.

#![deny(
    clippy::float_arithmetic,
    clippy::float_cmp,
    clippy::print_stdout,
    clippy::print_stderr
)]

pub mod catalog;
pub mod features;
pub mod regime;
pub mod strategy;
pub mod trend_pullback;
