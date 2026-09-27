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
//! - [`trend_pullback`]: a long pullback in an uptrend.
//! - [`trend_pullback_short`]: its mirror image, a short rally in a downtrend.
//! - [`breakout`]: a long 20-day-high breakout.
//! - [`mean_reversion`]: a long bounce from a stretched low inside a range.

#![deny(
    clippy::float_arithmetic,
    clippy::float_cmp,
    clippy::print_stdout,
    clippy::print_stderr
)]

pub mod breakout;
pub mod catalog;
pub mod features;
pub mod mean_reversion;
pub mod regime;
mod rules;
pub mod strategy;
pub mod trend_pullback;
pub mod trend_pullback_short;
