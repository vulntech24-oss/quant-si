//! QuantDesk server (spec §5.3 `qd-server`): configuration, wiring, startup
//! sequence and HTTP. The binary is the only place adapters meet ports.
//!
//! - [`ai`]: advisory AI wiring (shadow mode, INV-04).
//! - [`config`]: validated configuration; secrets from the environment only.
//! - [`startup`]: conservative startup (INV-07).
//! - [`http`]: health and readiness endpoints.
//! - [`paper`]: the paper runner and its optional daily schedule.
//! - [`SystemClock`]: the real clock, used only by binaries.

pub mod ai;
pub mod config;
pub mod http;
pub mod paper;
pub mod startup;

use chrono::{DateTime, Utc};
use qd_app::ports::Clock;

/// The system clock. Pure crates never see it; they get time through `Clock`.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}
