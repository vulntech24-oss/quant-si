//! Validation of registered strategy versions over the stored data, behind
//! the `Validator` port. The API and the CLI both use this.
//!
//! Loads the version from the registry and its implementation from the
//! catalog, which must have exactly the registered parameters (INV-10). Bars
//! are read as known now (INV-09). The protocol runs ([`crate::validation`]),
//! and the result, pass or fail, is recorded as append-only evidence
//! (INV-11) and audited.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{Duration, NaiveTime};
use qd_app::decision::StrategyVersionInfo;
use qd_app::evidence::{EvidenceKind, EvidenceRecord, EvidenceStore};
use qd_app::ports::{
    AuditLog, Clock, HistoricalMarketData, StoreError, ValidationRequest, Validator,
};
use qd_app::registry::StrategyRegistry;
use qd_domain::costs::CostModel;
use qd_domain::economics::SlippageAssumption;
use qd_domain::ids::{EvidenceId, InstrumentId};
use qd_domain::instrument::{InstrumentKind, InstrumentSpec, ProductType};
use qd_domain::market::{Bar, BarSeries};
use qd_domain::num::Money;
use qd_risk::config::RiskConfig;
use rust_decimal::Decimal;
use serde_json::json;

use crate::engine::InstrumentData;
use crate::validation::{ValidationCriteria, ValidationInput, validate};

/// Calendar days of history loaded before the first date for indicators.
const WARM_UP_DAYS: i64 = 400;

/// Validates versions over the stored data and records the evidence.
pub struct StoreValidator {
    market: Arc<dyn HistoricalMarketData>,
    registry: StrategyRegistry,
    evidence: Arc<dyn EvidenceStore>,
    audit: Arc<dyn AuditLog>,
    costs: Arc<dyn CostModel>,
    risk: RiskConfig,
    criteria: ValidationCriteria,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for StoreValidator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoreValidator")
            .field("criteria", &self.criteria)
            .finish_non_exhaustive()
    }
}

fn error(e: impl std::fmt::Display) -> StoreError {
    StoreError(e.to_string())
}

impl StoreValidator {
    /// Creates the validator. `risk` is the production configuration.
    #[allow(clippy::too_many_arguments)] // each is a distinct dependency
    pub fn new(
        market: Arc<dyn HistoricalMarketData>,
        registry: StrategyRegistry,
        evidence: Arc<dyn EvidenceStore>,
        audit: Arc<dyn AuditLog>,
        costs: Arc<dyn CostModel>,
        risk: RiskConfig,
        criteria: ValidationCriteria,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, StoreError> {
        criteria.validate().map_err(StoreError)?;
        Ok(Self {
            market,
            registry,
            evidence,
            audit,
            costs,
            risk,
            criteria,
            clock,
        })
    }

    async fn data(&self, request: &ValidationRequest) -> Result<Vec<InstrumentData>, StoreError> {
        let now = self.clock.now();
        let mut latest: HashMap<InstrumentId, InstrumentSpec> = HashMap::new();
        for spec in self.market.instruments(request.to).await? {
            let newer = latest
                .get(&spec.id)
                .is_none_or(|s| spec.version > s.version);
            if newer {
                latest.insert(spec.id, spec);
            }
        }
        for id in &request.instruments {
            if !latest.contains_key(id) {
                return Err(StoreError(format!("unknown instrument {id}")));
            }
        }
        let mut specs: Vec<InstrumentSpec> = latest
            .into_values()
            .filter(|s| request.instruments.is_empty() || request.instruments.contains(&s.id))
            .collect();
        specs.sort_by_key(|s| s.id);
        let mut data = Vec::new();
        for spec in specs {
            let bars = self
                .market
                .daily_bars(
                    spec.id,
                    request.from - Duration::days(WARM_UP_DAYS),
                    request.to,
                    now,
                )
                .await?;
            let Some(last) = bars.last().map(Bar::date) else {
                continue;
            };
            let series = BarSeries::new(spec.id, last, bars).map_err(error)?;
            let product = if spec.kind == InstrumentKind::Future {
                ProductType::Margin
            } else {
                ProductType::Delivery
            };
            data.push(InstrumentData {
                spec,
                product,
                series,
            });
        }
        if data.is_empty() {
            return Err(StoreError("no instrument has bars".to_owned()));
        }
        Ok(data)
    }
}

#[async_trait]
impl Validator for StoreValidator {
    async fn validate(
        &self,
        request: &ValidationRequest,
        actor: &str,
    ) -> Result<serde_json::Value, StoreError> {
        if request.from >= request.to || request.equity <= Decimal::ZERO {
            return Err(StoreError("invalid validation request".to_owned()));
        }
        let (version, stage) = self.registry.get(request.version).await.map_err(error)?;
        let catalog = qd_strategy::catalog::catalog().map_err(error)?;
        let entry = qd_strategy::catalog::find(
            &catalog,
            &version.reference.logic_version,
            &version.parameters,
        )
        .ok_or_else(|| {
            StoreError(
                "this build has no implementation with the registered logic version and parameters"
                    .to_owned(),
            )
        })?;
        let data = self.data(request).await?;
        let currency = data
            .first()
            .map(|d| d.spec.currency)
            .ok_or_else(|| StoreError("no data".to_owned()))?;
        let info = StrategyVersionInfo {
            reference: version.reference.clone(),
            stage,
            rr_floor: version.rr_floor,
            slippage: SlippageAssumption::new("slip-v1", Decimal::ZERO).map_err(error)?,
        };
        let input = ValidationInput {
            strategy: entry.strategy.as_ref(),
            info: &info,
            instruments: &data,
            from: request.from,
            to: request.to,
            equity: Money::new(request.equity, currency),
            slippage_ticks: Decimal::ONE,
            close_time_utc: NaiveTime::from_hms_opt(10, 0, 0).ok_or_else(|| error("bad time"))?,
        };
        let report = validate(&input, &self.criteria, &self.risk, self.costs.as_ref())
            .await
            .map_err(error)?;
        let now = self.clock.now();
        let record = EvidenceRecord {
            id: EvidenceId::new_at(now),
            version: request.version,
            kind: EvidenceKind::Validation,
            passed: report.passed,
            report: serde_json::to_value(&report).map_err(error)?,
            created_at: now,
        };
        self.evidence.record(&record).await?;
        self.audit
            .record(
                actor,
                "strategy.validate",
                json!({ "evidence": record.id, "version": record.version, "passed": record.passed }),
            )
            .await?;
        serde_json::to_value(&record).map_err(error)
    }
}
