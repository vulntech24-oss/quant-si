//! Health, alerts and metrics.

// Test code: a failed unwrap is a failed test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use qd_app::memory::InMemoryHaltStore;
use qd_app::monitor::{MonitorSettings, Severity, collect, prometheus};
use qd_app::ports::{HaltStore, PaperTrading, StoreError};
use qd_domain::halt::{Halt, HaltKind, HaltScope};
use qd_domain::ids::HaltId;
use serde_json::{Value, json};

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 10, 12, 0, 0).unwrap()
}

struct FakePaper(Value);

#[async_trait]
impl PaperTrading for FakePaper {
    async fn run_through(&self, _: chrono::NaiveDate) -> Result<Value, StoreError> {
        Err(StoreError("not in this test".to_owned()))
    }
    async fn state(&self) -> Result<Value, StoreError> {
        Ok(self.0.clone())
    }
}

struct BrokenHalts;

#[async_trait]
impl HaltStore for BrokenHalts {
    async fn load(&self) -> Result<Vec<Halt>, StoreError> {
        Err(StoreError("database down".to_owned()))
    }
    async fn record(&self, _: &Halt) -> Result<(), StoreError> {
        Err(StoreError("database down".to_owned()))
    }
}

#[tokio::test]
async fn a_healthy_book_raises_no_alert() {
    let paper = FakePaper(json!({
        "last_day": { "date": "2026-06-09" },
        "positions": [ { "state": "protected" }, { "state": "closed" } ],
        "working_orders": [ {}, {} ],
        "inconsistency": null
    }));
    let h = collect(
        &InMemoryHaltStore::new(),
        Some(&paper),
        None,
        now(),
        MonitorSettings::default(),
    )
    .await;
    assert!(h.alerts.is_empty(), "{:?}", h.alerts);
    assert!(!h.entries_halted);
    let p = h.paper.as_ref().unwrap();
    assert_eq!(
        (
            p.open_positions,
            p.unprotected_positions,
            p.working_orders,
            p.age_days
        ),
        (1, 0, 2, Some(1))
    );
    let text = prometheus(&h);
    assert!(text.contains("qd_up 1\n"));
    assert!(text.contains("qd_entries_halted 0\n"));
    assert!(text.contains("qd_paper_open_positions 1\n"));
}

#[tokio::test]
async fn problems_raise_alerts_most_severe_first() {
    let halts = InMemoryHaltStore::new();
    halts
        .record(
            &Halt::new(
                HaltId::new_at(now()),
                HaltKind::HardHalt,
                HaltScope::Global,
                "drawdown",
                now(),
                None,
                true,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let paper = FakePaper(json!({
        "last_day": { "date": "2026-06-01" },
        "positions": [ { "state": "unprotected" } ],
        "working_orders": [],
        "inconsistency": "order ledger and positions disagree"
    }));
    let h = collect(
        &halts,
        Some(&paper),
        None,
        now(),
        MonitorSettings::default(),
    )
    .await;
    let codes: Vec<&str> = h.alerts.iter().map(|a| a.code).collect();
    assert_eq!(
        codes,
        vec![
            "entries_halted",
            "paper_book_inconsistent",
            "unprotected_positions",
            "paper_stale"
        ]
    );
    assert_eq!(
        h.alerts[0].severity,
        Severity::Critical,
        "a hard halt is critical"
    );
    assert_eq!(h.manual_rearm_halts, 1);
    assert!(prometheus(&h).contains("qd_alerts_critical 3\n"));
}

#[tokio::test]
async fn invariant_06_an_unreadable_kill_switch_is_a_critical_alert_and_halts() {
    let h = collect(&BrokenHalts, None, None, now(), MonitorSettings::default()).await;
    assert!(h.entries_halted);
    assert!(!h.halt_state_known);
    assert_eq!(h.alerts[0].code, "halt_state_unknown");
    assert_eq!(h.alerts[0].severity, Severity::Critical);
}

#[tokio::test]
async fn an_unprotected_live_position_is_a_critical_alert() {
    let live = FakePaper(json!({
        "last_day": null,
        "positions": [ { "state": "unprotected" } ],
        "working_orders": [],
        "inconsistency": null
    }));
    let h = collect(
        &InMemoryHaltStore::new(),
        None,
        Some(&live),
        now(),
        MonitorSettings::default(),
    )
    .await;
    let codes: Vec<&str> = h.alerts.iter().map(|a| a.code).collect();
    assert_eq!(codes, vec!["live_unprotected_positions"]);
    assert!(prometheus(&h).contains("qd_live_unprotected_positions 1"));
    // A live book not configured is not watched.
    let off = FakePaper(json!({ "configured": false }));
    let h = collect(
        &InMemoryHaltStore::new(),
        None,
        Some(&off),
        now(),
        MonitorSettings::default(),
    )
    .await;
    assert!(h.live.is_none() && h.alerts.is_empty());
}

#[tokio::test]
async fn staleness_counts_trading_days_when_the_calendar_covers_them() {
    let set: qd_domain::calendar::CalendarSet =
        toml::from_str(include_str!("../../../config/calendars/india.toml")).unwrap();
    let calendars = qd_domain::calendar::Calendars::new(set).unwrap();
    let settings = MonitorSettings {
        calendar: calendars
            .get(&qd_domain::instrument::CalendarId("nse".to_owned()))
            .cloned(),
        ..MonitorSettings::default()
    };
    // Last day Thursday 1 Oct 2026; Friday 2 Oct is a holiday.
    let paper = FakePaper(json!({
        "last_day": { "date": "2026-10-01" },
        "positions": [],
        "working_orders": [],
        "inconsistency": null
    }));
    let at = |d| Utc.with_ymd_and_hms(2026, 10, d, 6, 0, 0).unwrap();
    let codes = |h: qd_app::monitor::Health| h.alerts.iter().map(|a| a.code).collect::<Vec<_>>();
    // Tuesday: only Monday is unprocessed, although five calendar days passed.
    let h = collect(
        &InMemoryHaltStore::new(),
        Some(&paper),
        None,
        at(6),
        settings.clone(),
    )
    .await;
    assert!(codes(h).is_empty());
    // Wednesday: Monday and Tuesday are unprocessed.
    let h = collect(
        &InMemoryHaltStore::new(),
        Some(&paper),
        None,
        at(7),
        settings,
    )
    .await;
    assert_eq!(codes(h), vec!["paper_stale"]);
}
