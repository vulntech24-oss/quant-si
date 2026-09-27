//! Health, alerts and metrics (ADR 0012).
//!
//! One place computes the operational state from the kill switch and the
//! paper book, and derives alerts from it. The server exposes it as
//! Prometheus text (`/metrics`, internal only), the API includes the alerts
//! in `/api/status`, and a background task logs alert changes as JSON log
//! events; the server forwards them to Telegram when notifications are on.
//! The live book (Zerodha), when configured, is watched like the paper book.

use chrono::{DateTime, NaiveDate, Utc};
use qd_domain::halt::HaltKind;
use serde::Serialize;
use serde_json::Value;

use crate::ports::{HaltStore, PaperTrading};

/// How bad an alert is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Worth knowing.
    Warning,
    /// Needs the owner now.
    Critical,
}

/// One alert.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
pub struct Alert {
    /// Severity.
    pub severity: Severity,
    /// Stable code.
    pub code: &'static str,
    /// What is wrong, for a human.
    pub message: String,
}

/// The paper book's operational state.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct PaperHealth {
    /// Last processed trading day.
    pub last_day: Option<NaiveDate>,
    /// Calendar days since it.
    pub age_days: Option<i64>,
    /// Whether the book restores consistently.
    pub consistent: bool,
    /// Open positions.
    pub open_positions: u32,
    /// Positions without valid protection.
    pub unprotected_positions: u32,
    /// Working orders.
    pub working_orders: u32,
}

/// The operational state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Health {
    /// When measured.
    pub at: DateTime<Utc>,
    /// Whether the halt store could be read.
    pub halt_state_known: bool,
    /// Active halts.
    pub active_halts: u32,
    /// Active halts that only a human can clear.
    pub manual_rearm_halts: u32,
    /// Whether new entries are blocked (unknown state counts as blocked).
    pub entries_halted: bool,
    /// Paper book, when paper trading is configured.
    pub paper: Option<PaperHealth>,
    /// Live book, when live trading is configured.
    pub live: Option<PaperHealth>,
    /// Alerts, most severe first.
    pub alerts: Vec<Alert>,
}

/// Alert thresholds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MonitorSettings {
    /// A paper book whose last day is older than this is stale.
    pub stale_after_days: i64,
}

impl Default for MonitorSettings {
    fn default() -> Self {
        // Covers a weekend plus a holiday.
        Self {
            stale_after_days: 4,
        }
    }
}

const OPEN_STATES: [&str; 5] = ["opening", "open", "protected", "unprotected", "exiting"];

fn paper_health(state: &Value, now: DateTime<Utc>) -> PaperHealth {
    let last_day: Option<NaiveDate> = state
        .pointer("/last_day/date")
        .and_then(|d| serde_json::from_value(d.clone()).ok());
    let positions = state
        .get("positions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let state_of = |p: &Value| {
        p.get("state")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    let count = |f: &dyn Fn(&Value) -> bool| {
        u32::try_from(positions.iter().filter(|p| f(p)).count()).unwrap_or(u32::MAX)
    };
    PaperHealth {
        last_day,
        age_days: last_day.map(|d| (now.date_naive() - d).num_days()),
        consistent: state.get("inconsistency").is_none_or(Value::is_null),
        open_positions: count(&|p| OPEN_STATES.contains(&state_of(p).as_str())),
        unprotected_positions: count(&|p| state_of(p) == "unprotected"),
        working_orders: u32::try_from(
            state
                .get("working_orders")
                .and_then(Value::as_array)
                .map_or(0, Vec::len),
        )
        .unwrap_or(u32::MAX),
    }
}

/// Measures the operational state and derives alerts.
pub async fn collect(
    halts: &dyn HaltStore,
    paper: Option<&dyn PaperTrading>,
    live: Option<&dyn PaperTrading>,
    now: DateTime<Utc>,
    settings: MonitorSettings,
) -> Health {
    let mut alerts = Vec::new();
    let (known, active, manual) = match halts.load().await {
        Ok(all) => {
            let active: Vec<_> = all.iter().filter(|h| h.is_active_at(now)).collect();
            let manual = active.iter().filter(|h| h.requires_manual_rearm()).count();
            if !active.is_empty() {
                let mut kinds: Vec<String> = active
                    .iter()
                    .map(|h| format!("{:?}", h.kind()).to_lowercase())
                    .collect();
                kinds.sort();
                kinds.dedup();
                let hard = active.iter().any(|h| h.kind() == HaltKind::HardHalt);
                alerts.push(Alert {
                    severity: if hard {
                        Severity::Critical
                    } else {
                        Severity::Warning
                    },
                    code: "entries_halted",
                    message: format!(
                        "New entries are halted by {} halt(s): {}.",
                        active.len(),
                        kinds.join(", ")
                    ),
                });
            }
            (true, active.len(), manual)
        }
        Err(e) => {
            alerts.push(Alert {
                severity: Severity::Critical,
                code: "halt_state_unknown",
                message: format!("The kill switch cannot be read, so entries are halted: {e}"),
            });
            (false, 0, 0)
        }
    };
    let paper = match paper {
        None => None,
        Some(p) => match p.state().await {
            // Paper trading switched off: no paper book to watch.
            Ok(state) if state.get("configured") == Some(&Value::Bool(false)) => None,
            Ok(state) => Some(paper_health(&state, now)),
            Err(e) => {
                alerts.push(Alert {
                    severity: Severity::Critical,
                    code: "paper_state_unreadable",
                    message: format!("The paper book cannot be read: {e}"),
                });
                None
            }
        },
    };
    if let Some(p) = &paper {
        if !p.consistent {
            alerts.push(Alert {
                severity: Severity::Critical,
                code: "paper_book_inconsistent",
                message: "The paper book does not restore consistently; paper runs are refused."
                    .to_owned(),
            });
        }
        if p.unprotected_positions > 0 {
            alerts.push(Alert {
                severity: Severity::Critical,
                code: "unprotected_positions",
                message: format!(
                    "{} position(s) have no valid protective stop.",
                    p.unprotected_positions
                ),
            });
        }
        match p.age_days {
            Some(age) if age > settings.stale_after_days => alerts.push(Alert {
                severity: Severity::Warning,
                code: "paper_stale",
                message: format!(
                    "The last paper day processed is {age} days old; import bars and run paper trading."
                ),
            }),
            None => alerts.push(Alert {
                severity: Severity::Warning,
                code: "paper_not_started",
                message: "No paper trading day has been processed yet.".to_owned(),
            }),
            _ => {}
        }
    }
    let live = match live {
        None => None,
        Some(l) => match l.state().await {
            Ok(state) if state.get("configured") == Some(&Value::Bool(false)) => None,
            Ok(state) => Some(paper_health(&state, now)),
            Err(e) => {
                alerts.push(Alert {
                    severity: Severity::Critical,
                    code: "live_state_unreadable",
                    message: format!("The live book cannot be read: {e}"),
                });
                None
            }
        },
    };
    if let Some(l) = &live {
        if !l.consistent {
            alerts.push(Alert {
                severity: Severity::Critical,
                code: "live_book_inconsistent",
                message: "The live book does not restore consistently; live runs are refused."
                    .to_owned(),
            });
        }
        if l.unprotected_positions > 0 {
            alerts.push(Alert {
                severity: Severity::Critical,
                code: "live_unprotected_positions",
                message: format!(
                    "{} live position(s) have no working protective stop at the broker.",
                    l.unprotected_positions
                ),
            });
        }
    }
    alerts.sort_by(|a, b| b.severity.cmp(&a.severity).then(a.code.cmp(b.code)));
    Health {
        at: now,
        halt_state_known: known,
        active_halts: u32::try_from(active).unwrap_or(u32::MAX),
        manual_rearm_halts: u32::try_from(manual).unwrap_or(u32::MAX),
        entries_halted: !known || active > 0,
        paper,
        live,
        alerts,
    }
}

/// Prometheus text exposition of the state.
#[must_use]
pub fn prometheus(h: &Health) -> String {
    let flag = |b: bool| u8::from(b);
    let mut out = String::new();
    let mut gauge = |name: &str, help: &str, value: String| {
        out.push_str(&format!(
            "# HELP {name} {help}\n# TYPE {name} gauge\n{name} {value}\n"
        ));
    };
    gauge("qd_up", "The server is running.", "1".to_owned());
    gauge(
        "qd_halt_state_known",
        "Whether the kill switch could be read.",
        flag(h.halt_state_known).to_string(),
    );
    gauge(
        "qd_entries_halted",
        "Whether new entries are blocked.",
        flag(h.entries_halted).to_string(),
    );
    gauge(
        "qd_active_halts",
        "Active halts.",
        h.active_halts.to_string(),
    );
    gauge(
        "qd_manual_rearm_halts",
        "Active halts that need a human re-arm.",
        h.manual_rearm_halts.to_string(),
    );
    let count = |s: Severity| {
        h.alerts
            .iter()
            .filter(|a| a.severity == s)
            .count()
            .to_string()
    };
    gauge(
        "qd_alerts_critical",
        "Critical alerts.",
        count(Severity::Critical),
    );
    gauge(
        "qd_alerts_warning",
        "Warning alerts.",
        count(Severity::Warning),
    );
    if let Some(p) = &h.paper {
        gauge(
            "qd_paper_book_consistent",
            "Whether the paper book restores consistently.",
            flag(p.consistent).to_string(),
        );
        if let Some(age) = p.age_days {
            gauge(
                "qd_paper_last_day_age_days",
                "Days since the last processed paper day.",
                age.to_string(),
            );
        }
        gauge(
            "qd_paper_open_positions",
            "Open paper positions.",
            p.open_positions.to_string(),
        );
        gauge(
            "qd_paper_unprotected_positions",
            "Paper positions without valid protection.",
            p.unprotected_positions.to_string(),
        );
        gauge(
            "qd_paper_working_orders",
            "Working paper orders.",
            p.working_orders.to_string(),
        );
    }
    if let Some(l) = &h.live {
        gauge(
            "qd_live_book_consistent",
            "Whether the live book restores consistently.",
            flag(l.consistent).to_string(),
        );
        gauge(
            "qd_live_open_positions",
            "Open live positions.",
            l.open_positions.to_string(),
        );
        gauge(
            "qd_live_unprotected_positions",
            "Live positions without a working protective stop.",
            l.unprotected_positions.to_string(),
        );
    }
    out
}
