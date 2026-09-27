//! The advisor interface and the input packet.
//!
//! The packet is built from a journaled decision record and holds the plan,
//! its economics and its explanation: what the owner sees. It never holds
//! account equity, quantities, credentials or anything secret (INV-15), so
//! a remote provider learns nothing it could act on.

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use qd_app::journal::{AiAdvice, AiStance};
use qd_domain::ids::{AiReviewId, DecisionId};
use rust_decimal::Decimal;
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

/// Longest summary kept.
pub const MAX_SUMMARY_CHARS: usize = 1000;
/// Most flags kept, and the longest flag.
pub const MAX_FLAGS: usize = 10;
/// Longest flag kept.
pub const MAX_FLAG_CHARS: usize = 200;

/// What an advisor sees of one decision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AdvisorInput {
    /// Decision.
    pub decision: DecisionId,
    /// Last completed trading date used.
    pub as_of_date: Option<NaiveDate>,
    /// Symbol.
    pub symbol: Option<String>,
    /// Post-risk outcome code: `enter`, `no_trade`, ...
    pub outcome: String,
    /// NO TRADE reason code, if any.
    pub reason_code: Option<String>,
    /// Market regime.
    pub regime: Option<String>,
    /// Setup type.
    pub setup_type: Option<String>,
    /// Entry price.
    pub entry: Option<Decimal>,
    /// Stop.
    pub stop: Option<Decimal>,
    /// Target.
    pub target: Option<Decimal>,
    /// Net reward-to-risk.
    pub rr_net: Option<Decimal>,
    /// Net risk per unit.
    pub risk_net: Option<Decimal>,
    /// Net reward per unit.
    pub reward_net: Option<Decimal>,
    /// Costs per unit.
    pub costs_per_unit: Option<Decimal>,
    /// Expected value in R.
    pub ev_r: Option<Decimal>,
    /// Target, stop, other probabilities.
    pub probabilities: Option<[Decimal; 3]>,
    /// Comparable setups behind the probabilities.
    pub evidence_count: Option<u32>,
    /// The strongest argument against the trade, from the proposal.
    pub argument_against: Option<String>,
}

fn dec(v: &Value, pointer: &str) -> Option<Decimal> {
    match v.pointer(pointer)? {
        Value::String(s) => s.parse().ok(),
        Value::Number(n) => n.to_string().parse().ok(),
        _ => None,
    }
}

fn text(v: &Value, pointer: &str) -> Option<String> {
    v.pointer(pointer)?.as_str().map(str::to_owned)
}

impl AdvisorInput {
    /// Builds the packet from a decision's journal JSON.
    #[must_use]
    pub fn from_decision(entry: &Value) -> Option<Self> {
        let probabilities = match (
            dec(entry, "/proposal/probabilities/p_target"),
            dec(entry, "/proposal/probabilities/p_stop"),
            dec(entry, "/proposal/probabilities/p_time"),
        ) {
            (Some(a), Some(b), Some(c)) => Some([a, b, c]),
            _ => None,
        };
        Some(Self {
            decision: serde_json::from_value(entry.get("id")?.clone()).ok()?,
            as_of_date: entry
                .get("as_of_date")
                .and_then(|d| serde_json::from_value(d.clone()).ok()),
            symbol: text(entry, "/proposal/instrument/symbol"),
            outcome: text(entry, "/outcome/outcome")?,
            reason_code: text(entry, "/outcome/reason/code"),
            regime: text(entry, "/regime"),
            setup_type: text(entry, "/proposal/setup_type"),
            entry: dec(entry, "/proposal/plan/entry/price"),
            stop: dec(entry, "/proposal/plan/stop"),
            target: dec(entry, "/proposal/plan/target"),
            rr_net: dec(entry, "/proposal/economics/rr_net"),
            risk_net: dec(entry, "/proposal/economics/risk_net"),
            reward_net: dec(entry, "/proposal/economics/reward_net"),
            costs_per_unit: dec(entry, "/proposal/economics/costs_per_unit"),
            ev_r: dec(entry, "/proposal/expected_value/in_r"),
            probabilities,
            evidence_count: entry
                .pointer("/proposal/probabilities/evidence_count")
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok()),
            argument_against: text(entry, "/proposal/explanation/strongest_argument_against"),
        })
    }
}

/// What an advisor returns, before validation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdviceDraft {
    /// Stance.
    pub stance: AiStance,
    /// Confidence, 0–1.
    pub confidence: Decimal,
    /// Summary.
    pub summary: String,
    /// Concerns.
    pub flags: Vec<String>,
}

/// Why an advisor produced nothing usable.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AdvisorError {
    /// The advisor failed (transport, provider error).
    #[error("advisor failed: {0}")]
    Failed(String),
    /// The call took too long.
    #[error("advisor timed out")]
    Timeout,
    /// The output broke the contract.
    #[error("invalid advice: {0}")]
    Invalid(String),
}

/// An advisor. Implementations only read the packet and return commentary.
#[async_trait]
pub trait Advisor: Send + Sync {
    /// Name and version, e.g. `checklist-v1`.
    fn name(&self) -> &str;
    /// Advises on one decision.
    async fn advise(&self, input: &AdvisorInput) -> Result<AdviceDraft, AdvisorError>;
}

fn clean(s: &str, max: usize) -> String {
    s.chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .take(max)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// Validates and bounds a draft into a journal record. Text is kept as
/// plain text; the frontend renders it with `textContent` only.
pub fn finalize(
    draft: AdviceDraft,
    advisor: &str,
    decision: DecisionId,
    at: DateTime<Utc>,
) -> Result<AiAdvice, AdvisorError> {
    if !(Decimal::ZERO..=Decimal::ONE).contains(&draft.confidence) {
        return Err(AdvisorError::Invalid(
            "confidence outside [0, 1]".to_owned(),
        ));
    }
    let summary = clean(&draft.summary, MAX_SUMMARY_CHARS);
    if summary.is_empty() {
        return Err(AdvisorError::Invalid("empty summary".to_owned()));
    }
    Ok(AiAdvice {
        id: AiReviewId::new_at(at),
        decision,
        advisor: clean(advisor, 64),
        stance: draft.stance,
        confidence: draft.confidence,
        summary,
        flags: draft
            .flags
            .iter()
            .map(|f| clean(f, MAX_FLAG_CHARS))
            .filter(|f| !f.is_empty())
            .take(MAX_FLAGS)
            .collect(),
        at,
    })
}
