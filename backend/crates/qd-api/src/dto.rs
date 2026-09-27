//! Response shapes. Decisions are summarized post-risk (INV-17): the headline
//! is the outcome after the Risk Gate, and a NO TRADE carries its reason.

use chrono::{DateTime, Utc};
use qd_app::ports::StoredJournalEntry;
use qd_domain::outcome::{DecisionOutcome, NoTradeReason};
use serde::Serialize;
use serde_json::Value;

/// A NO TRADE reason for display.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ReasonDto {
    /// Stable code.
    pub code: String,
    /// Typed detail.
    pub detail: Value,
}

/// One decision, as the decision list shows it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DecisionSummary {
    /// Journal sequence number.
    pub seq: i64,
    /// Decision id.
    pub id: Value,
    /// Decision time.
    pub at: Value,
    /// Last completed trading date used.
    pub as_of_date: Value,
    /// Instrument id.
    pub instrument_id: Value,
    /// Symbol, when a proposal was built.
    pub symbol: Option<String>,
    /// Strategy name.
    pub strategy: Value,
    /// Strategy version number.
    pub strategy_version: Value,
    /// Regime.
    pub regime: Value,
    /// Post-risk headline, e.g. "BUY (open long)" or "NO TRADE".
    pub headline: String,
    /// Why NO TRADE, if so.
    pub reason: Option<ReasonDto>,
    /// Setup type.
    pub setup_type: Option<Value>,
    /// Entry, stop and target, when a proposal was built.
    pub entry: Option<Value>,
    /// Stop.
    pub stop: Option<Value>,
    /// Target.
    pub target: Option<Value>,
    /// Net reward-to-risk.
    pub rr_net: Option<Value>,
    /// Expected value in R.
    pub ev_r: Option<Value>,
    /// Approved quantity, for entries.
    pub quantity: Option<Value>,
    /// When the journal recorded it.
    pub recorded_at: DateTime<Utc>,
}

fn at(value: &Value, path: &[&str]) -> Option<Value> {
    path.iter()
        .try_fold(value, |v, key| v.get(key))
        .filter(|v| !v.is_null())
        .cloned()
}

/// Builds a summary from a stored decision entry. Returns `None` for other kinds.
#[must_use]
pub fn decision_summary(stored: &StoredJournalEntry) -> Option<DecisionSummary> {
    if stored.kind != "decision" {
        return None;
    }
    let e = &stored.entry;
    let outcome: DecisionOutcome = serde_json::from_value(e.get("outcome")?.clone()).ok()?;
    let reason = match &outcome {
        DecisionOutcome::NoTrade { reason } => Some(reason_dto(reason)),
        _ => None,
    };
    Some(DecisionSummary {
        seq: stored.seq,
        id: at(e, &["id"])?,
        at: at(e, &["at"])?,
        as_of_date: at(e, &["as_of_date"])?,
        instrument_id: at(e, &["instrument"])?,
        symbol: at(e, &["proposal", "instrument", "symbol"])
            .and_then(|v| v.as_str().map(str::to_owned)),
        strategy: at(e, &["strategy", "name"])?,
        strategy_version: at(e, &["strategy", "version_number"])?,
        regime: at(e, &["regime"])?,
        headline: outcome.headline().to_owned(),
        reason,
        setup_type: at(e, &["proposal", "setup_type"]),
        entry: at(e, &["proposal", "plan", "entry", "price"]),
        stop: at(e, &["proposal", "plan", "stop"]),
        target: at(e, &["proposal", "plan", "target"]),
        rr_net: at(e, &["proposal", "economics", "rr_net"]),
        ev_r: at(e, &["proposal", "expected_value", "in_r"]),
        quantity: at(e, &["approval", "quantity"]),
        recorded_at: stored.recorded_at,
    })
}

fn reason_dto(reason: &NoTradeReason) -> ReasonDto {
    let detail = serde_json::to_value(reason)
        .ok()
        .and_then(|v| v.get("detail").cloned())
        .unwrap_or(Value::Null);
    ReasonDto {
        code: reason.code().to_owned(),
        detail,
    }
}
