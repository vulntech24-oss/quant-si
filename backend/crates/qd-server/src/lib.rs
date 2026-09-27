//! QuantDesk server (spec §5.3 `qd-server`): configuration, wiring, startup
//! sequence and HTTP. The binary is the only place adapters meet ports.
//!
//! - [`config`]: validated configuration; secrets from the environment only.
//! - [`startup`]: conservative startup (INV-07).
//! - [`http`]: health and readiness endpoints.
//! - [`runtime`]: effective settings (files + web UI) read on every run.
//! - [`paper`]: the paper runner and its optional daily schedule.
//! - [`data`]: instruments and bar uploads from the web UI.
//! - [`keys`]: crash-safe master-key rotation.
//! - [`kite`]: the Zerodha connection, live trading and their schedules.
//! - [`notify`]: Telegram notifications.
//! - [`SystemClock`]: the real clock, used only by binaries.

pub mod config;
pub mod data;
pub mod http;
pub mod keys;
pub mod kite;
pub mod notify;
pub mod paper;
pub mod runtime;
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
