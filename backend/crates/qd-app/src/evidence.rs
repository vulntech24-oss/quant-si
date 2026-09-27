//! Recorded evidence (INV-11) and the evidence tables the Decision Engine
//! reads (ADR 0010).
//!
//! - A **validation** record is produced by walk-forward, out-of-sample and
//!   holdout validation of a strategy version. A passed one backs
//!   `PassResearch` and the promotion to Paper, and its evidence tables give
//!   paper decisions their outcome probabilities.
//! - A **paper review** record is produced by reviewing a version's paper
//!   trades. A passed one backs promotions to the live stages.
//!
//! Records are append-only. Nothing edits a verdict after the fact.

use std::collections::{BTreeMap, HashMap};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use qd_domain::economics::OutcomeProbabilities;
use qd_domain::ids::{EvidenceId, StrategyVersionId};
use qd_domain::outcome::ExitReason;
use rust_decimal::{Decimal, RoundingStrategy};
use serde::{Deserialize, Serialize};

use crate::ports::{Evidence, EvidenceSource, StoreError};
use crate::session::TradeRecord;

/// What a piece of evidence is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    /// Offline validation (walk-forward, out-of-sample, holdout, Monte Carlo).
    Validation,
    /// Review of paper-trading results.
    PaperReview,
}

impl EvidenceKind {
    /// Stable code, as stored.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Validation => "validation",
            Self::PaperReview => "paper_review",
        }
    }
}

/// Outcome frequencies for one setup type, from out-of-sample trades.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceTable {
    /// Setup type.
    pub setup_type: String,
    /// Trades counted.
    pub count: u32,
    /// Share that hit the target first.
    pub p_target: Decimal,
    /// Share that hit the stop first.
    pub p_stop: Decimal,
    /// Share that ended any other way (time exit, invalidation, manual).
    pub p_time: Decimal,
    /// Mean R-multiple of the trades that ended another way.
    pub time_exit_r: Decimal,
}

/// Builds evidence tables from closed trades, per setup type.
///
/// Probabilities are rounded to four places on the conservative side: the
/// target share down, the stop share up, the rest to the other outcomes, so
/// they always sum to exactly one.
#[must_use]
pub fn evidence_tables(trades: &[TradeRecord]) -> Vec<EvidenceTable> {
    let mut groups: BTreeMap<&str, Vec<&TradeRecord>> = BTreeMap::new();
    for t in trades {
        groups.entry(t.setup_type.as_str()).or_default().push(t);
    }
    groups
        .into_iter()
        .filter_map(|(setup, ts)| {
            let n = Decimal::from(u32::try_from(ts.len()).ok()?);
            let targets = ts
                .iter()
                .filter(|t| t.exit_reason == ExitReason::TargetHit)
                .count();
            let stops = ts
                .iter()
                .filter(|t| t.exit_reason == ExitReason::StopHit)
                .count();
            let others: Vec<Decimal> = ts
                .iter()
                .filter(|t| !matches!(t.exit_reason, ExitReason::TargetHit | ExitReason::StopHit))
                .map(|t| t.r_multiple)
                .collect();
            let p_target = (Decimal::from(u32::try_from(targets).ok()?) / n)
                .round_dp_with_strategy(4, RoundingStrategy::ToZero);
            let p_stop = (Decimal::from(u32::try_from(stops).ok()?) / n)
                .round_dp_with_strategy(4, RoundingStrategy::AwayFromZero)
                .min(Decimal::ONE - p_target);
            let time_exit_r = if others.is_empty() {
                Decimal::ZERO
            } else {
                (others.iter().sum::<Decimal>() / Decimal::from(u32::try_from(others.len()).ok()?))
                    .round_dp(4)
            };
            Some(EvidenceTable {
                setup_type: setup.to_owned(),
                count: u32::try_from(ts.len()).ok()?,
                p_target,
                p_stop,
                p_time: Decimal::ONE - p_target - p_stop,
                time_exit_r,
            })
        })
        .collect()
}

/// One recorded piece of evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRecord {
    /// Id: what promotions cite.
    pub id: EvidenceId,
    /// The strategy version it is about.
    pub version: StrategyVersionId,
    /// Kind.
    pub kind: EvidenceKind,
    /// Whether it met every pass criterion.
    pub passed: bool,
    /// The full report.
    pub report: serde_json::Value,
    /// When it was recorded.
    pub created_at: DateTime<Utc>,
}

impl EvidenceRecord {
    /// The evidence tables in a validation report, if any.
    #[must_use]
    pub fn tables(&self) -> Vec<EvidenceTable> {
        self.report
            .get("evidence")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default()
    }
}

/// Append-only storage of evidence records.
#[async_trait]
pub trait EvidenceStore: Send + Sync {
    /// Records a new piece of evidence.
    async fn record(&self, record: &EvidenceRecord) -> Result<(), StoreError>;
    /// Loads one record.
    async fn get(&self, id: EvidenceId) -> Result<Option<EvidenceRecord>, StoreError>;
    /// Every record, newest first, optionally for one version.
    async fn list(
        &self,
        version: Option<StrategyVersionId>,
    ) -> Result<Vec<EvidenceRecord>, StoreError>;
}

/// Evidence tables from the latest passed validation of each version.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StoredEvidence {
    tables: HashMap<(StrategyVersionId, String), EvidenceTable>,
}

impl StoredEvidence {
    /// Builds the source from records (any order).
    #[must_use]
    pub fn from_records(records: &[EvidenceRecord]) -> Self {
        let mut latest: HashMap<StrategyVersionId, &EvidenceRecord> = HashMap::new();
        for r in records
            .iter()
            .filter(|r| r.kind == EvidenceKind::Validation && r.passed)
        {
            let newer = latest
                .get(&r.version)
                .is_none_or(|l| r.created_at > l.created_at);
            if newer {
                latest.insert(r.version, r);
            }
        }
        let mut tables = HashMap::new();
        for (version, record) in latest {
            for table in record.tables() {
                tables.insert((version, table.setup_type.clone()), table);
            }
        }
        Self { tables }
    }

    /// Loads every record from a store.
    pub async fn load(store: &dyn EvidenceStore) -> Result<Self, StoreError> {
        Ok(Self::from_records(&store.list(None).await?))
    }
}

impl EvidenceSource for StoredEvidence {
    fn evidence(&self, version: StrategyVersionId, setup_type: &str) -> Option<Evidence> {
        let t = self.tables.get(&(version, setup_type.to_owned()))?;
        Some(Evidence {
            probabilities: OutcomeProbabilities::new(
                t.p_target,
                t.p_stop,
                t.p_time,
                "oos-empirical-v1",
                t.count,
            )
            .ok()?,
            time_exit_r: t.time_exit_r,
        })
    }
}

/// Loads the evidence a run decides with. A run loads once at its start, so
/// evidence recorded during a run applies from the next one.
#[async_trait]
pub trait EvidenceLoader: Send + Sync {
    /// The current evidence.
    async fn load(&self) -> Result<std::sync::Arc<dyn EvidenceSource>, StoreError>;
}

/// Evidence tables from the latest passed validation of each version.
pub struct StoreEvidenceLoader(pub std::sync::Arc<dyn EvidenceStore>);

impl std::fmt::Debug for StoreEvidenceLoader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoreEvidenceLoader")
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl EvidenceLoader for StoreEvidenceLoader {
    async fn load(&self) -> Result<std::sync::Arc<dyn EvidenceSource>, StoreError> {
        Ok(std::sync::Arc::new(
            StoredEvidence::load(self.0.as_ref()).await?,
        ))
    }
}

/// A fixed evidence source (tests, and runs that must not see stored evidence).
pub struct FixedEvidence(pub std::sync::Arc<dyn EvidenceSource>);

impl std::fmt::Debug for FixedEvidence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FixedEvidence").finish_non_exhaustive()
    }
}

#[async_trait]
impl EvidenceLoader for FixedEvidence {
    async fn load(&self) -> Result<std::sync::Arc<dyn EvidenceSource>, StoreError> {
        Ok(self.0.clone())
    }
}
