//! Research backtests of stored data, behind the `BacktestRunner` port.
//!
//! Loads the latest instrument spec and point-in-time bars (known now), runs
//! the requested catalog strategy (default `trend-pullback-1.0.0`) simulated at the Paper stage with the neutral
//! evidence prior and the research risk configuration (ADR 0006), and returns
//! the report as JSON. The API and the CLI both use this.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{Duration, NaiveTime};
use qd_app::decision::{EvidencePolicy, StrategyVersionInfo};
use qd_app::ports::{
    BacktestRequest, BacktestRunner, Clock, Evidence, EvidenceSource, HistoricalMarketData,
    StoreError,
};
use qd_domain::costs::CostModel;
use qd_domain::economics::SlippageAssumption;
use qd_domain::ids::{StrategyId, StrategyVersionId};
use qd_domain::instrument::{InstrumentKind, ProductType};
use qd_domain::lifecycle::strategy::StrategyStage;
use qd_domain::market::{Bar, BarSeries};
use qd_domain::num::Money;
use qd_domain::proposal::StrategyRef;
use qd_risk::config::RiskConfig;
use qd_strategy::trend_pullback::TrendPullback;
use rust_decimal::Decimal;

use crate::engine::{BacktestConfig, InstrumentData, research_risk_config, run_backtest};

/// Bars of history loaded before the first decision date for indicator warm-up.
const WARM_UP_DAYS: i64 = 400;

struct NoEvidence;

impl EvidenceSource for NoEvidence {
    fn evidence(&self, _: StrategyVersionId, _: &str) -> Option<Evidence> {
        None
    }
}

/// Research backtests over the stored market data.
pub struct ResearchBacktester {
    market: Arc<dyn HistoricalMarketData>,
    costs: Arc<dyn CostModel>,
    risk: RiskConfig,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for ResearchBacktester {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResearchBacktester").finish_non_exhaustive()
    }
}

impl ResearchBacktester {
    /// Creates the runner. `risk` is the production configuration; the
    /// research copy (minimum EV 0) is derived from it.
    pub fn new(
        market: Arc<dyn HistoricalMarketData>,
        costs: Arc<dyn CostModel>,
        risk: &RiskConfig,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, StoreError> {
        let risk = research_risk_config(risk)
            .ok_or_else(|| StoreError("invalid research risk configuration".to_owned()))?;
        Ok(Self {
            market,
            costs,
            risk,
            clock,
        })
    }
}

fn error(e: impl std::fmt::Display) -> StoreError {
    StoreError(e.to_string())
}

#[async_trait]
impl BacktestRunner for ResearchBacktester {
    async fn run(&self, request: &BacktestRequest) -> Result<serde_json::Value, StoreError> {
        if request.from > request.to || request.equity <= Decimal::ZERO {
            return Err(StoreError("invalid backtest request".to_owned()));
        }
        let now = self.clock.now();
        let spec = self
            .market
            .instruments(request.to)
            .await?
            .into_iter()
            .filter(|s| s.id == request.instrument)
            .max_by_key(|s| s.version)
            .ok_or_else(|| StoreError("unknown instrument".to_owned()))?;
        let bars = self
            .market
            .daily_bars(
                request.instrument,
                request.from - Duration::days(WARM_UP_DAYS),
                request.to,
                now,
            )
            .await?;
        let last = bars
            .last()
            .map(Bar::date)
            .ok_or_else(|| StoreError("no bars".to_owned()))?;
        let series = BarSeries::new(spec.id, last, bars).map_err(error)?;
        let catalog = qd_strategy::catalog::catalog().map_err(error)?;
        let wanted = request
            .logic_version
            .as_deref()
            .unwrap_or(TrendPullback::LOGIC_VERSION);
        let entry = catalog
            .iter()
            .find(|c| c.strategy.logic_version() == wanted)
            .ok_or_else(|| StoreError(format!("{wanted} is not in this build")))?;
        let product = if spec.kind == InstrumentKind::Future {
            ProductType::Margin
        } else {
            ProductType::Delivery
        };
        let config = BacktestConfig {
            initial_equity: Money::new(request.equity, spec.currency),
            start: request.from,
            end: request.to,
            strategy: StrategyVersionInfo {
                reference: StrategyRef {
                    strategy_id: StrategyId::new_at(now),
                    name: entry.strategy.name().to_owned(),
                    version_id: StrategyVersionId::new_at(now),
                    version_number: 0,
                    logic_version: wanted.to_owned(),
                    git_sha: "research".to_owned(),
                },
                // Research runs simulate the version at Paper (ADR 0006).
                stage: StrategyStage::Paper,
                rr_floor: entry.rr_floor,
                slippage: SlippageAssumption::new("slip-v1", spec.tick_size).map_err(error)?,
            },
            evidence: EvidencePolicy::ResearchPrior,
            slippage_ticks: Decimal::ONE,
            close_time_utc: NaiveTime::from_hms_opt(10, 0, 0).ok_or_else(|| error("bad time"))?,
            calendar_version: "unversioned".to_owned(),
            plan_slippage_ticks: None,
        };
        let data = [InstrumentData {
            spec,
            product,
            series,
        }];
        let report = run_backtest(
            entry.strategy.as_ref(),
            &data,
            &config,
            &self.risk,
            self.costs.as_ref(),
            &NoEvidence,
        )
        .await
        .map_err(error)?;
        serde_json::to_value(serde_json::json!({
            "metrics": report.metrics,
            "decisions": report.decisions,
            "trades": report.trades,
            "equity_curve": report.equity_curve,
            "open_positions": report.open_positions,
            "unprotected_events": report.unprotected_events,
            "reconciliation_mismatches": report.reconciliation_mismatches,
        }))
        .map_err(error)
    }
}

/// Walk-forward parameter searches over the stored market data.
pub struct ResearchSearch {
    market: Arc<dyn HistoricalMarketData>,
    costs: Arc<dyn CostModel>,
    risk: RiskConfig,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for ResearchSearch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResearchSearch").finish_non_exhaustive()
    }
}

impl ResearchSearch {
    /// Creates the runner with the production risk configuration.
    #[must_use]
    pub fn new(
        market: Arc<dyn HistoricalMarketData>,
        costs: Arc<dyn CostModel>,
        risk: RiskConfig,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            market,
            costs,
            risk,
            clock,
        }
    }
}

#[async_trait]
impl qd_app::ports::ParameterSearch for ResearchSearch {
    async fn search(
        &self,
        request: &qd_app::ports::SearchRequest,
    ) -> Result<serde_json::Value, StoreError> {
        let settings = crate::search::SearchSettings::default();
        if request.from > request.to
            || request.equity <= Decimal::ZERO
            || request.instruments.is_empty()
            || request.instruments.len() > 10
        {
            return Err(StoreError(
                "invalid search request (1 to 10 instruments, positive equity, from ≤ to)"
                    .to_owned(),
            ));
        }
        let candidates = crate::search::grid(&request.logic_version)
            .ok_or_else(|| StoreError(format!("no search grid for {}", request.logic_version)))?;
        let now = self.clock.now();
        let specs = self.market.instruments(request.to).await?;
        let mut data = Vec::new();
        let mut currency = None;
        for id in &request.instruments {
            let spec = specs
                .iter()
                .filter(|s| s.id == *id)
                .max_by_key(|s| s.version)
                .cloned()
                .ok_or_else(|| StoreError(format!("unknown instrument {id}")))?;
            let bars = self
                .market
                .daily_bars(
                    *id,
                    request.from - Duration::days(settings.train_days + WARM_UP_DAYS),
                    request.to,
                    now,
                )
                .await?;
            let Some(last) = bars.last().map(Bar::date) else {
                continue;
            };
            currency.get_or_insert(spec.currency);
            let product = if spec.kind == InstrumentKind::Future {
                ProductType::Margin
            } else {
                ProductType::Delivery
            };
            let series = BarSeries::new(spec.id, last, bars).map_err(error)?;
            data.push(InstrumentData {
                spec,
                product,
                series,
            });
        }
        let currency = currency.ok_or_else(|| StoreError("no bars".to_owned()))?;
        let name = qd_strategy::catalog::catalog()
            .map_err(error)?
            .iter()
            .find(|c| c.strategy.logic_version() == request.logic_version)
            .map_or_else(
                || request.logic_version.clone(),
                |c| c.strategy.name().to_owned(),
            );
        let info = StrategyVersionInfo {
            reference: StrategyRef {
                strategy_id: StrategyId::new_at(now),
                name,
                version_id: StrategyVersionId::new_at(now),
                version_number: 0,
                logic_version: request.logic_version.clone(),
                git_sha: "search".to_owned(),
            },
            stage: StrategyStage::Paper,
            rr_floor: Decimal::ZERO,
            slippage: SlippageAssumption::new("slip-v1", Decimal::ZERO).map_err(error)?,
        };
        let report = crate::search::walk_forward_search(
            &crate::search::SearchInput {
                info: &info,
                instruments: &data,
                from: request.from,
                to: request.to,
                equity: Money::new(request.equity, currency),
                slippage_ticks: Decimal::ONE,
            },
            &candidates,
            settings,
            &self.risk,
            self.costs.as_ref(),
        )
        .await
        .map_err(error)?;
        serde_json::to_value(report).map_err(error)
    }
}
