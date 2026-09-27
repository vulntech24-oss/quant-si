//! Runs advisors over new entry decisions, in shadow mode.
//!
//! - Only `enter` decisions are sent to advisors (NO TRADE needs no second
//!   opinion, and this bounds provider cost).
//! - Each advisor advises once per decision; re-runs skip what is advised.
//! - A daily call budget per advisor (counted from today's journaled
//!   advice) and a timeout per call. When the budget runs out, the rest waits
//!   for the next day. Nothing retries in a loop.
//! - Failures are counted and logged, never retried in the same run, and
//!   never affect trading: the only write is the `ai_advice` journal entry.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::NaiveDate;
use qd_app::journal::{AiAdvice, JournalEntry};
use qd_app::ports::{Clock, Journal, JournalReader, StoreError};
use qd_domain::ids::DecisionId;
use serde::{Deserialize, Serialize};

use crate::advisor::{Advisor, AdvisorError, AdvisorInput, finalize};

/// Orchestrator settings (`[ai]` in the server configuration).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiSettings {
    /// Calls per advisor per UTC day.
    pub max_calls_per_day: u32,
    /// Timeout per call, in seconds.
    pub timeout_seconds: u64,
}

/// What a run did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct AiRunReport {
    /// Entry decisions seen.
    pub entry_decisions: u32,
    /// Advice written.
    pub advice_written: u32,
    /// Skipped because already advised.
    pub already_advised: u32,
    /// Skipped because the advisor's daily budget ran out.
    pub over_budget: u32,
    /// Failures (advisor, decision, error).
    pub failures: Vec<String>,
}

/// The orchestrator.
pub struct AiOrchestrator {
    advisors: Vec<Arc<dyn Advisor>>,
    journal: Arc<dyn Journal>,
    reader: Arc<dyn JournalReader>,
    clock: Arc<dyn Clock>,
    settings: AiSettings,
    usage: Mutex<HashMap<String, (NaiveDate, u32)>>,
}

impl std::fmt::Debug for AiOrchestrator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AiOrchestrator")
            .field(
                "advisors",
                &self.advisors.iter().map(|a| a.name()).collect::<Vec<_>>(),
            )
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

/// Every decision and advice entry in the journal, oldest first.
pub async fn load(
    reader: &dyn JournalReader,
) -> Result<(Vec<serde_json::Value>, Vec<AiAdvice>), StoreError> {
    let mut decisions = Vec::new();
    let mut advice = Vec::new();
    let mut after = 0;
    loop {
        let page = reader
            .replay(&["decision", "ai_advice"], after, 1000)
            .await?;
        let Some(last) = page.last() else { break };
        after = last.seq;
        let full = page.len() >= 1000;
        for e in page {
            if e.kind == "decision" {
                decisions.push(e.entry);
            } else if let Ok(a) = serde_json::from_value::<AiAdvice>(e.entry) {
                advice.push(a);
            }
        }
        if !full {
            break;
        }
    }
    Ok((decisions, advice))
}

impl AiOrchestrator {
    /// Creates the orchestrator. Its only write capability is the journal.
    #[must_use]
    pub fn new(
        advisors: Vec<Arc<dyn Advisor>>,
        journal: Arc<dyn Journal>,
        reader: Arc<dyn JournalReader>,
        clock: Arc<dyn Clock>,
        settings: AiSettings,
    ) -> Self {
        Self {
            advisors,
            journal,
            reader,
            clock,
            settings,
            usage: Mutex::new(HashMap::new()),
        }
    }

    /// Takes one call from an advisor's daily budget, if any is left.
    fn take_budget(&self, advisor: &str) -> bool {
        let today = self.clock.now().date_naive();
        let mut usage = self
            .usage
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = usage.entry(advisor.to_owned()).or_insert((today, 0));
        if entry.0 != today {
            *entry = (today, 0);
        }
        if entry.1 >= self.settings.max_calls_per_day {
            return false;
        }
        entry.1 += 1;
        true
    }

    /// Advises on every entry decision not yet advised by each advisor.
    pub async fn run(&self) -> Result<AiRunReport, StoreError> {
        let (decisions, existing) = load(self.reader.as_ref()).await?;
        // The budget counts today's journaled advice, so it holds across
        // restarts and across orchestrators built per run.
        {
            let today = self.clock.now().date_naive();
            let mut usage = self
                .usage
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            usage.clear();
            for a in existing.iter().filter(|a| a.at.date_naive() == today) {
                let entry = usage.entry(a.advisor.clone()).or_insert((today, 0));
                entry.1 += 1;
            }
        }
        let advised: HashSet<(DecisionId, String)> = existing
            .iter()
            .map(|a| (a.decision, a.advisor.clone()))
            .collect();
        let mut report = AiRunReport::default();
        for entry in &decisions {
            let Some(input) = AdvisorInput::from_decision(entry) else {
                continue;
            };
            if input.outcome != "enter" {
                continue;
            }
            report.entry_decisions += 1;
            for advisor in &self.advisors {
                let name = advisor.name().to_owned();
                if advised.contains(&(input.decision, name.clone())) {
                    report.already_advised += 1;
                    continue;
                }
                if !self.take_budget(&name) {
                    report.over_budget += 1;
                    continue;
                }
                let call = tokio::time::timeout(
                    Duration::from_secs(self.settings.timeout_seconds),
                    advisor.advise(&input),
                )
                .await
                .unwrap_or(Err(AdvisorError::Timeout));
                let advice =
                    call.and_then(|draft| finalize(draft, &name, input.decision, self.clock.now()));
                match advice {
                    Ok(advice) => {
                        self.journal
                            .append(&JournalEntry::AiAdvice(Box::new(advice)))
                            .await
                            .map_err(|e| StoreError(e.to_string()))?;
                        report.advice_written += 1;
                    }
                    Err(e) => {
                        tracing::warn!(advisor = %name, decision = %input.decision, error = %e, "advisor failed");
                        report
                            .failures
                            .push(format!("{name} on {}: {e}", input.decision));
                    }
                }
            }
        }
        Ok(report)
    }
}
