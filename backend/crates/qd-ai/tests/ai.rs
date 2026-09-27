//! Advisory AI: checklist, output validation, orchestration budget and
//! timeouts, scoring, and the structural INV-04 guarantees.

// Test code: a failed unwrap is a failed test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use qd_ai::advisor::{AdviceDraft, Advisor, AdvisorError, AdvisorInput, finalize};
use qd_ai::checklist::{ChecklistAdvisor, ChecklistConfig};
use qd_ai::orchestrator::{AiOrchestrator, AiSettings};
use qd_ai::scorecard::scorecard;
use qd_app::journal::{AiAdvice, AiStance, JournalEntry};
use qd_app::ports::{Clock, Journal, JournalError, JournalReader, StoreError, StoredJournalEntry};
use qd_app::session::TradeRecord;
use qd_domain::action::Side;
use qd_domain::ids::{AiReviewId, DecisionId, PositionId};
use qd_domain::num::Quantity;
use qd_domain::outcome::ExitReason;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde_json::{Value, json};

fn at() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap()
}

struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        at()
    }
}

/// A journal that can also be read back, in memory.
#[derive(Default)]
struct MemJournal(Mutex<Vec<(String, Value)>>);

impl MemJournal {
    fn push(&self, kind: &str, entry: Value) {
        self.0.lock().unwrap().push((kind.to_owned(), entry));
    }
    fn kinds(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .map(|(k, _)| k.clone())
            .collect()
    }
}

#[async_trait]
impl Journal for MemJournal {
    async fn append(&self, entry: &JournalEntry) -> Result<u64, JournalError> {
        let v = serde_json::to_value(entry).unwrap();
        let kind = v["kind"].as_str().unwrap().to_owned();
        self.push(&kind, v);
        Ok(self.0.lock().unwrap().len() as u64)
    }
}

#[async_trait]
impl JournalReader for MemJournal {
    async fn recent(
        &self,
        _: Option<&str>,
        _: Option<i64>,
        _: i64,
    ) -> Result<Vec<StoredJournalEntry>, StoreError> {
        Ok(vec![])
    }
    async fn after(&self, _: i64, _: i64) -> Result<Vec<StoredJournalEntry>, StoreError> {
        Ok(vec![])
    }
    async fn replay(
        &self,
        kinds: &[&str],
        after: i64,
        limit: i64,
    ) -> Result<Vec<StoredJournalEntry>, StoreError> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .iter()
            .enumerate()
            .map(|(i, (k, v))| (i as i64 + 1, k, v))
            .filter(|(seq, k, _)| *seq > after && kinds.contains(&k.as_str()))
            .take(limit as usize)
            .map(|(seq, k, v)| StoredJournalEntry {
                seq,
                kind: k.clone(),
                entry: v.clone(),
                recorded_at: at(),
            })
            .collect())
    }
    async fn decision(&self, _: DecisionId) -> Result<Option<StoredJournalEntry>, StoreError> {
        Ok(None)
    }
}

fn decision(outcome: &str, rr: &str, ev: &str, evidence: u64) -> (DecisionId, Value) {
    let id = DecisionId::new_at(at());
    (
        id,
        json!({
            "kind": "decision",
            "id": id,
            "as_of_date": "2026-05-29",
            "regime": "trend_up",
            "outcome": { "outcome": outcome },
            "proposal": {
                "instrument": { "symbol": "TEST-EQ" },
                "setup_type": "pullback_in_uptrend",
                "plan": { "entry": { "price": "100" }, "stop": "95", "target": "112" },
                "economics": { "rr_net": rr, "risk_net": "5.2", "reward_net": "11.6", "costs_per_unit": "0.2" },
                "expected_value": { "in_r": ev },
                "probabilities": { "p_target": "0.45", "p_stop": "0.35", "p_time": "0.2", "evidence_count": evidence },
                "explanation": { "strongest_argument_against": "late in the trend" }
            }
        }),
    )
}

fn input(entry: &Value) -> AdvisorInput {
    AdvisorInput::from_decision(entry).unwrap()
}

#[tokio::test]
async fn the_checklist_agrees_cautions_or_disagrees_deterministically() {
    let advisor = ChecklistAdvisor::new(ChecklistConfig::default());
    let comfortable = input(&decision("enter", "2.2", "0.45", 120).1);
    let draft = advisor.advise(&comfortable).await.unwrap();
    assert_eq!((draft.stance, draft.flags.len()), (AiStance::Agree, 0));

    let thin = input(&decision("enter", "1.6", "0.05", 35).1);
    let draft = advisor.advise(&thin).await.unwrap();
    assert_eq!(draft.stance, AiStance::Disagree, "{:?}", draft.flags);
    assert_eq!(draft.flags.len(), 3);
    assert_eq!(advisor.advise(&thin).await.unwrap(), draft, "deterministic");

    let one = input(&decision("enter", "1.8", "0.45", 120).1);
    assert_eq!(
        advisor.advise(&one).await.unwrap().stance,
        AiStance::Caution
    );

    let no_trade = input(&decision("no_trade", "2.2", "0.45", 120).1);
    assert_eq!(
        advisor.advise(&no_trade).await.unwrap().stance,
        AiStance::Abstain
    );
}

#[test]
fn advice_is_validated_and_bounded() {
    let decision = DecisionId::new_at(at());
    let draft = |confidence, summary: &str, flags: Vec<String>| AdviceDraft {
        stance: AiStance::Caution,
        confidence,
        summary: summary.to_owned(),
        flags,
    };
    assert!(finalize(draft(dec!(1.2), "x", vec![]), "a", decision, at()).is_err());
    assert!(finalize(draft(dec!(0.5), " \u{7} ", vec![]), "a", decision, at()).is_err());
    let long = "y".repeat(5000);
    let many: Vec<String> = (0..50).map(|i| format!("flag {i}\u{1b}[31m")).collect();
    let advice = finalize(draft(dec!(0.5), &long, many), "a", decision, at()).unwrap();
    assert_eq!(
        advice.summary.chars().count(),
        qd_ai::advisor::MAX_SUMMARY_CHARS
    );
    assert_eq!(advice.flags.len(), qd_ai::advisor::MAX_FLAGS);
    assert!(advice.flags.iter().all(|f| !f.contains('\u{1b}')));
}

struct SlowAdvisor;

#[async_trait]
impl Advisor for SlowAdvisor {
    fn name(&self) -> &str {
        "slow-v1"
    }
    async fn advise(&self, _: &AdvisorInput) -> Result<AdviceDraft, AdvisorError> {
        tokio::time::sleep(Duration::from_secs(5)).await;
        Err(AdvisorError::Failed("unreachable".to_owned()))
    }
}

struct BrokenAdvisor;

#[async_trait]
impl Advisor for BrokenAdvisor {
    fn name(&self) -> &str {
        "broken-v1"
    }
    async fn advise(&self, _: &AdvisorInput) -> Result<AdviceDraft, AdvisorError> {
        Err(AdvisorError::Failed("provider error".to_owned()))
    }
}

fn orchestrator(
    journal: &Arc<MemJournal>,
    advisors: Vec<Arc<dyn Advisor>>,
    max_calls: u32,
) -> AiOrchestrator {
    AiOrchestrator::new(
        advisors,
        journal.clone(),
        journal.clone(),
        Arc::new(FixedClock),
        AiSettings {
            max_calls_per_day: max_calls,
            timeout_seconds: 1,
        },
    )
}

#[tokio::test]
async fn invariant_04_the_orchestrator_only_appends_advice_once_per_advisor() {
    let journal = Arc::new(MemJournal::default());
    for d in [
        decision("enter", "2.2", "0.45", 120),
        decision("enter", "1.6", "0.05", 35),
        decision("no_trade", "2.2", "0.45", 120),
    ] {
        journal.push("decision", d.1);
    }
    let before = journal.kinds();
    let o = orchestrator(
        &journal,
        vec![
            Arc::new(ChecklistAdvisor::default()),
            Arc::new(BrokenAdvisor),
            Arc::new(SlowAdvisor),
        ],
        100,
    );
    let report = o.run().await.unwrap();
    assert_eq!(
        report.entry_decisions, 2,
        "NO TRADE is never sent to advisors"
    );
    assert_eq!(report.advice_written, 2);
    // A broken or slow advisor fails alone; the others still advise.
    assert_eq!(report.failures.len(), 4, "{:?}", report.failures);
    assert!(report.failures.iter().any(|f| f.contains("timed out")));
    // The only new entries are advice entries (shadow mode).
    let after = journal.kinds();
    assert_eq!(&after[..before.len()], &before[..]);
    assert!(after[before.len()..].iter().all(|k| k == "ai_advice"));

    // A re-run advises nothing twice.
    let again = o.run().await.unwrap();
    assert_eq!(again.advice_written, 0);
    assert_eq!(again.already_advised, 2);
}

#[tokio::test]
async fn a_daily_budget_caps_calls_per_advisor() {
    let journal = Arc::new(MemJournal::default());
    for _ in 0..3 {
        journal.push("decision", decision("enter", "2.2", "0.45", 120).1);
    }
    let o = orchestrator(&journal, vec![Arc::new(ChecklistAdvisor::default())], 2);
    let report = o.run().await.unwrap();
    assert_eq!((report.advice_written, report.over_budget), (2, 1));
    // Same day: still over budget; nothing is lost, it waits for tomorrow.
    let report = o.run().await.unwrap();
    assert_eq!((report.advice_written, report.over_budget), (0, 1));
}

fn trade(decision: DecisionId, r: Decimal) -> TradeRecord {
    TradeRecord {
        position: PositionId::new_at(at()),
        decision: Some(decision),
        setup_type: "s".to_owned(),
        instrument: "X".to_owned(),
        side: Side::Long,
        opened_on: NaiveDate::from_ymd_opt(2026, 6, 1).unwrap(),
        closed_on: NaiveDate::from_ymd_opt(2026, 6, 9).unwrap(),
        quantity: Quantity::new(Decimal::ONE).unwrap(),
        entry_price: dec!(100),
        exit_price: dec!(100),
        gross_pnl: r,
        costs: Decimal::ZERO,
        net_pnl: r,
        r_multiple: r,
        exit_reason: ExitReason::TimeExit,
    }
}

fn advice(decision: DecisionId, stance: AiStance) -> AiAdvice {
    AiAdvice {
        id: AiReviewId::new_at(at()),
        decision,
        advisor: "checklist-v1".to_owned(),
        stance,
        confidence: dec!(0.6),
        summary: "s".to_owned(),
        flags: vec![],
        at: at(),
    }
}

#[test]
fn the_scorecard_counts_stances_that_matched_outcomes() {
    let d: Vec<DecisionId> = (0..4).map(|_| DecisionId::new_at(at())).collect();
    let advice = [
        advice(d[0], AiStance::Agree),    // win: right
        advice(d[1], AiStance::Agree),    // loss: wrong
        advice(d[2], AiStance::Disagree), // loss: right
        advice(d[3], AiStance::Abstain),  // never scored
    ];
    let trades = [
        trade(d[0], dec!(1.5)),
        trade(d[1], dec!(-1)),
        trade(d[2], dec!(-1)),
        trade(d[3], dec!(2)),
    ];
    let s = &scorecard(&advice, &trades)[0];
    assert_eq!((s.advised, s.scored, s.right), (4, 3, 2));
    assert_eq!((s.agree_trades, s.agree_mean_r), (2, dec!(0.25)));
    assert_eq!((s.doubt_trades, s.doubt_mean_r), (1, dec!(-1)));
}

#[test]
fn invariant_04_the_ai_crate_cannot_reach_orders_limits_halts_or_credentials() {
    // Dependencies: nothing that can place orders or change trading state.
    let manifest = include_str!("../Cargo.toml");
    let deps = manifest
        .split("[dependencies]")
        .nth(1)
        .unwrap()
        .split("[dev-dependencies]")
        .next()
        .unwrap();
    for forbidden in [
        "qd-broker",
        "qd-store",
        "qd-risk",
        "qd-backtest",
        "qd-server",
        "qd-api",
        "qd-strategy",
    ] {
        assert!(
            !deps.contains(forbidden),
            "qd-ai must not depend on {forbidden}"
        );
    }
    // Code: no use of the components that act on trading state.
    for (file, source) in [
        ("advisor.rs", include_str!("../src/advisor.rs")),
        ("checklist.rs", include_str!("../src/checklist.rs")),
        ("orchestrator.rs", include_str!("../src/orchestrator.rs")),
        ("scorecard.rs", include_str!("../src/scorecard.rs")),
        ("service.rs", include_str!("../src/service.rs")),
    ] {
        for forbidden in [
            "OrderGateway",
            "PositionManager",
            "EntryAuthorization",
            "BrokerOrderExecutor",
            "HaltStore",
            "RiskGate",
            "RiskConfig",
            "AccountStore",
            "StrategyRegistry",
            "Secret",
        ] {
            assert!(!source.contains(forbidden), "{file} mentions {forbidden}");
        }
    }
}
