//! Review and calibration of paper (later live) results (ADR 0010).
//!
//! Joins each journaled entry decision with the trade it produced (through
//! the decision id in `day_closed` trades) and compares prediction with
//! outcome, per strategy version:
//!
//! - predicted outcome shares (target, stop, other) against realized shares;
//! - the Brier score of the target prediction, `mean((p_target − hit)²)`;
//! - calibration bins of `p_target`;
//! - predicted EV in R against realized mean R.
//!
//! A **paper review** applies pass criteria (`config/review.toml`). A passed
//! review is the only evidence that can back a promotion to a live stage
//! (INV-11); the owner still approves with step-up, and every INV-14 live
//! condition still applies.
//!
//! All arithmetic is decimal.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::NaiveDate;
use qd_domain::ids::{AccountId, DecisionId, EvidenceId, StrategyVersionId};
use qd_domain::outcome::ExitReason;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::evidence::{EvidenceKind, EvidenceRecord, EvidenceStore};
use crate::ports::{AuditLog, Clock, JournalReader, StoreError};
use crate::session::{DayRecord, TradeRecord};

/// Paper-review pass criteria (`config/review.toml`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewCriteria {
    /// Minimum closed paper trades of the version.
    pub min_trades: u32,
    /// Minimum paper trading days processed.
    pub min_days: u32,
    /// Minimum realized mean net R.
    pub min_expectancy_r: Decimal,
    /// How far realized mean R may trail the predicted mean EV.
    pub max_expectancy_shortfall_r: Decimal,
    /// Maximum drawdown of the paper account.
    pub max_drawdown: Decimal,
    /// Maximum Brier score of the target prediction.
    pub max_brier: Decimal,
}

impl ReviewCriteria {
    /// Range checks.
    pub fn validate(&self) -> Result<(), String> {
        let unit = |d: Decimal| (Decimal::ZERO..=Decimal::ONE).contains(&d);
        if !unit(self.max_drawdown) || !unit(self.max_brier) {
            return Err("review: max_drawdown and max_brier must be between 0 and 1".to_owned());
        }
        if self.max_expectancy_shortfall_r < Decimal::ZERO {
            return Err("review: max_expectancy_shortfall_r must not be negative".to_owned());
        }
        Ok(())
    }
}

/// A predicted-vs-realized bin of `p_target`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CalibrationBin {
    /// Lower edge (inclusive).
    pub from: Decimal,
    /// Upper edge (exclusive; the last bin includes 1).
    pub to: Decimal,
    /// Trades in the bin.
    pub count: u32,
    /// Mean predicted target probability.
    pub predicted: Decimal,
    /// Realized share of target hits.
    pub realized: Decimal,
}

/// Per-version review.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct VersionReview {
    /// Version.
    pub version: StrategyVersionId,
    /// Entry decisions journaled.
    pub entries: u32,
    /// Closed trades joined to their decisions.
    pub trades: u32,
    /// Mean predicted target / stop / other shares.
    pub predicted: [Decimal; 3],
    /// Realized target / stop / other shares.
    pub realized: [Decimal; 3],
    /// Brier score of the target prediction.
    pub brier: Decimal,
    /// Mean predicted EV in R.
    pub predicted_ev_r: Decimal,
    /// Realized mean net R.
    pub realized_r: Decimal,
    /// Calibration bins (width 0.2).
    pub bins: Vec<CalibrationBin>,
}

/// One pass criterion and its outcome.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReviewCheck {
    /// Criterion.
    pub name: &'static str,
    /// Passed.
    pub passed: bool,
    /// Measured value against the threshold.
    pub detail: String,
}

/// The review of an account's paper results.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReviewReport {
    /// Account.
    pub account: AccountId,
    /// Paper days processed.
    pub days: u32,
    /// First and last processed day.
    pub period: Option<(NaiveDate, NaiveDate)>,
    /// Maximum drawdown of the account's daily equity.
    pub max_drawdown: Decimal,
    /// Days with reconciliation mismatches or unprotected positions.
    pub operational_incidents: u32,
    /// Per version.
    pub versions: Vec<VersionReview>,
}

/// What a decision predicted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prediction {
    /// Decision.
    pub decision: DecisionId,
    /// Version.
    pub version: StrategyVersionId,
    /// Target, stop, other probabilities.
    pub p: [Decimal; 3],
    /// EV in R.
    pub ev_r: Decimal,
}

fn dec_path(v: &Value, path: &[&str]) -> Option<Decimal> {
    let leaf = path.iter().try_fold(v, |v, k| v.get(k))?;
    match leaf {
        Value::String(s) => s.parse().ok(),
        Value::Number(n) => n.to_string().parse().ok(),
        _ => None,
    }
}

/// Reads an entry decision's prediction from its journal JSON. `None` for
/// other accounts, NO TRADE decisions and malformed entries.
#[must_use]
pub fn prediction(account: AccountId, entry: &Value) -> Option<Prediction> {
    let acct: AccountId = serde_json::from_value(entry.get("account")?.clone()).ok()?;
    if acct != account || entry.pointer("/outcome/outcome")?.as_str()? != "enter" {
        return None;
    }
    let proposal = entry.get("proposal")?;
    Some(Prediction {
        decision: serde_json::from_value(entry.get("id")?.clone()).ok()?,
        version: serde_json::from_value(entry.pointer("/strategy/version_id")?.clone()).ok()?,
        p: [
            dec_path(proposal, &["probabilities", "p_target"])?,
            dec_path(proposal, &["probabilities", "p_stop"])?,
            dec_path(proposal, &["probabilities", "p_time"])?,
        ],
        ev_r: dec_path(proposal, &["expected_value", "in_r"])?,
    })
}

fn share(n: usize, total: usize) -> Decimal {
    if total == 0 {
        return Decimal::ZERO;
    }
    (Decimal::from(u64::try_from(n).unwrap_or(0))
        / Decimal::from(u64::try_from(total).unwrap_or(1)))
    .round_dp(4)
}

fn mean(values: impl Iterator<Item = Decimal>) -> Decimal {
    let (sum, n) = values.fold((Decimal::ZERO, 0_u64), |(s, n), v| (s + v, n + 1));
    if n == 0 {
        Decimal::ZERO
    } else {
        (sum / Decimal::from(n)).round_dp(4)
    }
}

const fn outcome_index(reason: ExitReason) -> usize {
    match reason {
        ExitReason::TargetHit => 0,
        ExitReason::StopHit => 1,
        _ => 2,
    }
}

/// Builds the review from predictions and processed days (oldest first).
#[must_use]
pub fn review(account: AccountId, predictions: &[Prediction], days: &[DayRecord]) -> ReviewReport {
    let by_decision: HashMap<DecisionId, &Prediction> =
        predictions.iter().map(|p| (p.decision, p)).collect();
    let trades: Vec<(&Prediction, &TradeRecord)> = days
        .iter()
        .flat_map(|d| d.trades.iter())
        .filter_map(|t| Some((*by_decision.get(&t.decision?)?, t)))
        .collect();

    let mut peak = Decimal::ZERO;
    let mut max_drawdown = Decimal::ZERO;
    for d in days {
        peak = peak.max(d.equity);
        if peak > Decimal::ZERO {
            max_drawdown = max_drawdown.max((peak - d.equity) / peak);
        }
    }

    let mut versions: BTreeMap<StrategyVersionId, Vec<(&Prediction, &TradeRecord)>> =
        BTreeMap::new();
    for p in predictions {
        versions.entry(p.version).or_default();
    }
    for pair in &trades {
        versions.entry(pair.0.version).or_default().push(*pair);
    }
    let reviews = versions
        .into_iter()
        .map(|(version, pairs)| {
            let n = pairs.len();
            let mut realized_counts = [0_usize; 3];
            for (_, t) in &pairs {
                realized_counts[outcome_index(t.exit_reason)] += 1;
            }
            let bins = (0..5)
                .map(|i| {
                    let from = Decimal::new(i * 2, 1);
                    let to = Decimal::new(i * 2 + 2, 1);
                    let inside: Vec<&(&Prediction, &TradeRecord)> = pairs
                        .iter()
                        .filter(|(p, _)| {
                            p.p[0] >= from && (p.p[0] < to || (i == 4 && p.p[0] <= to))
                        })
                        .collect();
                    CalibrationBin {
                        from,
                        to,
                        count: u32::try_from(inside.len()).unwrap_or(u32::MAX),
                        predicted: mean(inside.iter().map(|(p, _)| p.p[0])),
                        realized: share(
                            inside
                                .iter()
                                .filter(|(_, t)| t.exit_reason == ExitReason::TargetHit)
                                .count(),
                            inside.len(),
                        ),
                    }
                })
                .collect();
            VersionReview {
                version,
                entries: u32::try_from(predictions.iter().filter(|p| p.version == version).count())
                    .unwrap_or(u32::MAX),
                trades: u32::try_from(n).unwrap_or(u32::MAX),
                predicted: [0, 1, 2].map(|k| mean(pairs.iter().map(|(p, _)| p.p[k]))),
                realized: realized_counts.map(|c| share(c, n)),
                brier: mean(pairs.iter().map(|(p, t)| {
                    let hit = if t.exit_reason == ExitReason::TargetHit {
                        Decimal::ONE
                    } else {
                        Decimal::ZERO
                    };
                    (p.p[0] - hit) * (p.p[0] - hit)
                })),
                predicted_ev_r: mean(pairs.iter().map(|(p, _)| p.ev_r)),
                realized_r: mean(pairs.iter().map(|(_, t)| t.r_multiple)),
                bins,
            }
        })
        .collect();
    ReviewReport {
        account,
        days: u32::try_from(days.len()).unwrap_or(u32::MAX),
        period: days.first().zip(days.last()).map(|(a, b)| (a.date, b.date)),
        max_drawdown: max_drawdown.round_dp(6),
        operational_incidents: u32::try_from(
            days.iter()
                .filter(|d| d.mismatches > 0 || d.unprotected > 0)
                .count(),
        )
        .unwrap_or(u32::MAX),
        versions: reviews,
    }
}

/// Applies the pass criteria to one version's review.
#[must_use]
pub fn paper_checks(
    report: &ReviewReport,
    version: &VersionReview,
    c: &ReviewCriteria,
) -> Vec<ReviewCheck> {
    let check = |name, passed, detail| ReviewCheck {
        name,
        passed,
        detail,
    };
    vec![
        check(
            "paper_trades",
            version.trades >= c.min_trades,
            format!(
                "{} closed paper trades, need {}",
                version.trades, c.min_trades
            ),
        ),
        check(
            "paper_days",
            report.days >= c.min_days,
            format!("{} paper days, need {}", report.days, c.min_days),
        ),
        check(
            "expectancy",
            version.trades > 0 && version.realized_r >= c.min_expectancy_r,
            format!(
                "{}R realized, need {}R",
                version.realized_r, c.min_expectancy_r
            ),
        ),
        check(
            "prediction_shortfall",
            version.trades > 0
                && version.predicted_ev_r - version.realized_r <= c.max_expectancy_shortfall_r,
            format!(
                "predicted {}R, realized {}R, allowed shortfall {}R",
                version.predicted_ev_r, version.realized_r, c.max_expectancy_shortfall_r
            ),
        ),
        check(
            "brier",
            version.trades > 0 && version.brier <= c.max_brier,
            format!("Brier {}, limit {}", version.brier, c.max_brier),
        ),
        check(
            "drawdown",
            report.max_drawdown <= c.max_drawdown,
            format!(
                "account drawdown {}, limit {}",
                report.max_drawdown, c.max_drawdown
            ),
        ),
        check(
            "operations",
            report.operational_incidents == 0,
            format!(
                "{} days with reconciliation mismatches or unprotected positions",
                report.operational_incidents
            ),
        ),
    ]
}

/// Reviews over the journal, for the API and the CLI.
#[async_trait]
pub trait Reviewer: Send + Sync {
    /// The review of the configured account, as JSON.
    async fn review(&self) -> Result<Value, StoreError>;
    /// Records a paper review of one version as evidence (pass or fail) and
    /// returns the record as JSON.
    async fn record(&self, version: StrategyVersionId, actor: &str) -> Result<Value, StoreError>;
}

/// The journal-backed reviewer.
pub struct JournalReviewer {
    /// Journal.
    pub reader: Arc<dyn JournalReader>,
    /// Evidence store.
    pub evidence: Arc<dyn EvidenceStore>,
    /// Audit log.
    pub audit: Arc<dyn AuditLog>,
    /// Clock.
    pub clock: Arc<dyn Clock>,
    /// The account reviewed.
    pub account: AccountId,
    /// Pass criteria.
    pub criteria: ReviewCriteria,
}

impl std::fmt::Debug for JournalReviewer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JournalReviewer")
            .field("account", &self.account)
            .finish_non_exhaustive()
    }
}

impl JournalReviewer {
    async fn report(&self) -> Result<ReviewReport, StoreError> {
        let mut predictions = Vec::new();
        let mut days = Vec::new();
        let mut after = 0;
        loop {
            let page = self
                .reader
                .replay(&["decision", "day_closed"], after, 1000)
                .await?;
            let Some(last) = page.last() else { break };
            after = last.seq;
            let full = page.len() >= 1000;
            for e in page {
                if e.kind == "decision" {
                    predictions.extend(prediction(self.account, &e.entry));
                } else if let Ok(d) = serde_json::from_value::<DayRecord>(e.entry) {
                    if d.account == self.account {
                        days.push(d);
                    }
                }
            }
            if !full {
                break;
            }
        }
        Ok(review(self.account, &predictions, &days))
    }
}

#[async_trait]
impl Reviewer for JournalReviewer {
    async fn review(&self) -> Result<Value, StoreError> {
        let report = self.report().await?;
        let checks: Vec<Value> = report
            .versions
            .iter()
            .map(|v| json!({ "version": v.version, "checks": paper_checks(&report, v, &self.criteria) }))
            .collect();
        Ok(json!({ "report": report, "criteria": self.criteria, "checks": checks }))
    }

    async fn record(&self, version: StrategyVersionId, actor: &str) -> Result<Value, StoreError> {
        let report = self.report().await?;
        let v = report
            .versions
            .iter()
            .find(|v| v.version == version)
            .ok_or_else(|| StoreError("no paper decisions for this version".to_owned()))?;
        let checks = paper_checks(&report, v, &self.criteria);
        let now = self.clock.now();
        let record = EvidenceRecord {
            id: EvidenceId::new_at(now),
            version,
            kind: EvidenceKind::PaperReview,
            passed: checks.iter().all(|c| c.passed),
            report: json!({
                "account": report.account,
                "days": report.days,
                "period": report.period,
                "max_drawdown": report.max_drawdown,
                "operational_incidents": report.operational_incidents,
                "version": v,
                "criteria": self.criteria,
                "checks": checks,
            }),
            created_at: now,
        };
        self.evidence.record(&record).await?;
        self.audit
            .record(
                actor,
                "strategy.paper_review",
                json!({ "evidence": record.id, "version": version, "passed": record.passed }),
            )
            .await?;
        serde_json::to_value(&record).map_err(|e| StoreError(e.to_string()))
    }
}
