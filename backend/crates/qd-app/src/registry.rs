//! Strategy Registry (spec §5.2, INV-10, INV-11).
//!
//! A registered version's parameters and logic never change; the store
//! rejects updates. Its stage is the replay of its append-only stage events
//! through the domain state machine, so an illegal transition can never be
//! stored. Promotions need an owner approval backed by recorded evidence;
//! automatic demotion needs none.
//!
//! Evidence is checked, not just cited (INV-11, ADR 0010):
//!
//! - `PassResearch` and the promotion to Paper need a **passed validation**
//!   of this version.
//! - Promotions to SmallCapital and Full need a **passed paper review** of
//!   this version. Until one exists, no version can reach a live stage.

use std::sync::Arc;

use async_trait::async_trait;
use qd_domain::ids::EvidenceId;
use qd_domain::ids::StrategyVersionId;
use qd_domain::lifecycle::strategy::{StageEvent, StrategyStage, TradingStage};
use qd_domain::proposal::StrategyRef;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::evidence::{EvidenceKind, EvidenceStore};
use crate::ports::{AuditLog, StoreError};

/// A registered, immutable strategy version.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StrategyVersionRecord {
    /// Identity, logic version and git SHA.
    pub reference: StrategyRef,
    /// Parameters, as registered.
    pub parameters: serde_json::Value,
    /// Net RR floor.
    pub rr_floor: Decimal,
}

/// Storage for the registry.
#[async_trait]
pub trait StrategyRegistryStore: Send + Sync {
    /// Registers a new version. Fails if the id or (strategy, number) exists.
    async fn register(&self, version: &StrategyVersionRecord) -> Result<(), StoreError>;
    /// Loads a version.
    async fn version(
        &self,
        id: StrategyVersionId,
    ) -> Result<Option<StrategyVersionRecord>, StoreError>;
    /// All versions.
    async fn versions(&self) -> Result<Vec<StrategyVersionRecord>, StoreError>;
    /// Stage events of a version, oldest first.
    async fn stage_events(&self, id: StrategyVersionId) -> Result<Vec<StageEvent>, StoreError>;
    /// Appends a stage event and the stage it leads to.
    async fn append_stage_event(
        &self,
        id: StrategyVersionId,
        event: &StageEvent,
        stage: StrategyStage,
    ) -> Result<(), StoreError>;
}

/// Why a registry operation failed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RegistryError {
    /// Storage failed.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// The version does not exist.
    #[error("unknown strategy version")]
    UnknownVersion,
    /// The transition is illegal from the current stage.
    #[error("{0}")]
    IllegalTransition(String),
    /// Stored events do not replay (the history is inconsistent).
    #[error("stored stage history does not replay: {0}")]
    CorruptHistory(String),
    /// The cited evidence does not back the event.
    #[error("evidence does not back this event: {0}")]
    Evidence(String),
}

/// The Strategy Registry.
#[derive(Clone)]
pub struct StrategyRegistry {
    store: Arc<dyn StrategyRegistryStore>,
    audit: Arc<dyn AuditLog>,
    evidence: Arc<dyn EvidenceStore>,
}

impl std::fmt::Debug for StrategyRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StrategyRegistry").finish_non_exhaustive()
    }
}

impl StrategyRegistry {
    /// Creates the registry.
    #[must_use]
    pub fn new(
        store: Arc<dyn StrategyRegistryStore>,
        audit: Arc<dyn AuditLog>,
        evidence: Arc<dyn EvidenceStore>,
    ) -> Self {
        Self {
            store,
            audit,
            evidence,
        }
    }

    async fn check_evidence(
        &self,
        version: StrategyVersionId,
        cited: EvidenceId,
        kind: EvidenceKind,
    ) -> Result<(), RegistryError> {
        let record =
            self.evidence.get(cited).await?.ok_or_else(|| {
                RegistryError::Evidence(format!("evidence {cited} is not recorded"))
            })?;
        if record.version != version {
            return Err(RegistryError::Evidence(format!(
                "evidence {cited} is about another version"
            )));
        }
        if record.kind != kind {
            return Err(RegistryError::Evidence(format!(
                "evidence {cited} is a {}, this needs a {}",
                record.kind.code(),
                kind.code()
            )));
        }
        if !record.passed {
            return Err(RegistryError::Evidence(format!(
                "evidence {cited} did not pass"
            )));
        }
        Ok(())
    }

    /// Registers a new version in `Draft`.
    pub async fn register(
        &self,
        version: &StrategyVersionRecord,
        actor: &str,
    ) -> Result<(), RegistryError> {
        self.store.register(version).await?;
        self.audit
            .record(
                actor,
                "strategy.register",
                serde_json::json!({ "version_id": version.reference.version_id }),
            )
            .await?;
        Ok(())
    }

    /// The version and its current stage.
    pub async fn get(
        &self,
        id: StrategyVersionId,
    ) -> Result<(StrategyVersionRecord, StrategyStage), RegistryError> {
        let version = self
            .store
            .version(id)
            .await?
            .ok_or(RegistryError::UnknownVersion)?;
        Ok((version, self.stage(id).await?))
    }

    /// Every registered version.
    pub async fn versions(&self) -> Result<Vec<StrategyVersionRecord>, RegistryError> {
        Ok(self.store.versions().await?)
    }

    /// Current stage: the replay of the stored events from `Draft`.
    pub async fn stage(&self, id: StrategyVersionId) -> Result<StrategyStage, RegistryError> {
        self.store
            .stage_events(id)
            .await?
            .iter()
            .try_fold(StrategyStage::Draft, |stage, event| stage.apply(event))
            .map_err(|e| RegistryError::CorruptHistory(e.to_string()))
    }

    /// Applies a stage event after validating it against the current stage.
    pub async fn transition(
        &self,
        id: StrategyVersionId,
        event: &StageEvent,
        actor: &str,
    ) -> Result<StrategyStage, RegistryError> {
        if self.store.version(id).await?.is_none() {
            return Err(RegistryError::UnknownVersion);
        }
        let current = self.stage(id).await?;
        let next = current
            .apply(event)
            .map_err(|e| RegistryError::IllegalTransition(e.to_string()))?;
        match event {
            StageEvent::PassResearch { evidence } => {
                self.check_evidence(id, *evidence, EvidenceKind::Validation)
                    .await?;
            }
            StageEvent::Promote { to, approval } => {
                let kind = match to {
                    TradingStage::Paper => EvidenceKind::Validation,
                    TradingStage::SmallCapital | TradingStage::Full => EvidenceKind::PaperReview,
                };
                self.check_evidence(id, approval.evidence, kind).await?;
            }
            _ => {}
        }
        self.store.append_stage_event(id, event, next).await?;
        self.audit
            .record(
                actor,
                "strategy.transition",
                serde_json::json!({ "version_id": id, "event": event, "stage": next }),
            )
            .await?;
        Ok(next)
    }
}
