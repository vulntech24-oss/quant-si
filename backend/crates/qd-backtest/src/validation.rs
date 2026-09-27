//! Walk-forward, out-of-sample and holdout validation (ADR 0010).
//!
//! The protocol for a strategy version whose parameters are fixed by its
//! logic version (nothing is fitted, so there is no in-sample optimization):
//!
//! 1. Split `[from, to]`: the last `holdout_fraction` is the **holdout**,
//!    and the rest is the development period.
//! 2. **Walk forward** over the development period in consecutive windows of
//!    `window_days`. Each window is an independent run from a flat account,
//!    with all earlier bars as indicator history and no later bar ever
//!    visible (point-in-time series, INV-09). Every window is out of sample
//!    for a strategy that was not fitted on it.
//! 3. The **holdout** runs once, after the windows, as the final check.
//! 4. **Evidence tables** come from the out-of-sample trades only. The
//!    holdout stays independent of the numbers that paper decisions use.
//! 5. **Monte Carlo** resamples the out-of-sample R-multiples at the
//!    configured risk per trade.
//! 6. The version **passes** only if every check passes.
//!
//! Runs use the research configuration (neutral evidence prior, minimum EV
//! 0; ADR 0006): validation measures what the setups do, it does not assume it.
//! Positions still open at a window's end are not counted as trades.

use chrono::{Duration, NaiveDate};
use qd_app::decision::{EvidencePolicy, StrategyVersionInfo};
use qd_app::evidence::{EvidenceTable, evidence_tables};
use qd_app::ports::{Evidence, EvidenceSource};
use qd_domain::costs::CostModel;
use qd_domain::ids::StrategyVersionId;
use qd_domain::num::Money;
use qd_risk::config::RiskConfig;
use qd_strategy::strategy::Strategy;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::engine::{
    BacktestConfig, BacktestError, InstrumentData, research_risk_config, run_backtest,
};
use crate::metrics::{Metrics, TradeRecord};
use crate::montecarlo::{MonteCarloConfig, MonteCarloReport, monte_carlo};

/// Pass criteria and protocol settings (`config/validation.toml`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ValidationCriteria {
    /// Walk-forward window length, calendar days.
    pub window_days: i64,
    /// Share of the date range held out for the final check.
    pub holdout_fraction: Decimal,
    /// Minimum out-of-sample trades.
    pub min_oos_trades: u32,
    /// Minimum mean net R out of sample.
    pub min_oos_expectancy_r: Decimal,
    /// Minimum share of windows (with trades) whose expectancy is positive.
    pub min_positive_window_share: Decimal,
    /// Maximum drawdown inside any window.
    pub max_window_drawdown: Decimal,
    /// Minimum holdout trades.
    pub min_holdout_trades: u32,
    /// Minimum mean net R in the holdout.
    pub min_holdout_expectancy_r: Decimal,
    /// Maximum Monte Carlo 95th-percentile drawdown.
    pub max_mc_drawdown_p95: Decimal,
    /// Maximum share of Monte Carlo paths reaching the hard-halt drawdown.
    pub max_mc_prob_over_halt: Decimal,
    /// Monte Carlo paths.
    pub monte_carlo_paths: u32,
    /// Monte Carlo seed.
    pub monte_carlo_seed: u64,
}

impl ValidationCriteria {
    /// Checks the settings themselves.
    pub fn validate(&self) -> Result<(), String> {
        let unit = |d: Decimal| (Decimal::ZERO..=Decimal::ONE).contains(&d);
        let valid = self.window_days >= 20
            && self.holdout_fraction > Decimal::ZERO
            && self.holdout_fraction < Decimal::ONE
            && unit(self.min_positive_window_share)
            && unit(self.max_window_drawdown)
            && unit(self.max_mc_drawdown_p95)
            && unit(self.max_mc_prob_over_halt)
            && self.monte_carlo_paths > 0;
        if !valid {
            return Err("validation criteria out of range".to_owned());
        }
        Ok(())
    }
}

/// One walk-forward window.
#[derive(Clone, Debug, Serialize)]
pub struct WindowResult {
    /// First date.
    pub start: NaiveDate,
    /// Last date.
    pub end: NaiveDate,
    /// Metrics of the window's run.
    pub metrics: Metrics,
    /// Positions still open at the end (not counted).
    pub open_positions: usize,
}

/// One pass criterion and its outcome.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Check {
    /// Criterion.
    pub name: &'static str,
    /// Whether it passed.
    pub passed: bool,
    /// The measured value against the threshold.
    pub detail: String,
}

/// The full validation report (stored as the evidence record's report).
#[derive(Clone, Debug, Serialize)]
pub struct ValidationReport {
    /// Version validated.
    pub version: StrategyVersionId,
    /// Logic version.
    pub logic_version: String,
    /// Instruments (symbols).
    pub instruments: Vec<String>,
    /// First date.
    pub from: NaiveDate,
    /// Holdout start.
    pub holdout_from: NaiveDate,
    /// Last date.
    pub to: NaiveDate,
    /// Criteria applied.
    pub criteria: ValidationCriteria,
    /// Walk-forward windows.
    pub windows: Vec<WindowResult>,
    /// Out-of-sample metrics (all window trades).
    pub oos: Metrics,
    /// Holdout metrics.
    pub holdout: Metrics,
    /// Monte Carlo on the out-of-sample trades.
    pub monte_carlo: Option<MonteCarloReport>,
    /// Evidence tables from the out-of-sample trades.
    pub evidence: Vec<EvidenceTable>,
    /// Out-of-sample trades.
    pub oos_trades: Vec<TradeRecord>,
    /// Every check.
    pub checks: Vec<Check>,
    /// Whether every check passed.
    pub passed: bool,
}

/// A validation run's inputs.
#[derive(Clone, Copy)]
pub struct ValidationInput<'a> {
    /// The strategy logic.
    pub strategy: &'a dyn Strategy,
    /// The version (its stage is irrelevant to research runs).
    pub info: &'a StrategyVersionInfo,
    /// Instruments with all their stored bars.
    pub instruments: &'a [InstrumentData],
    /// First date.
    pub from: NaiveDate,
    /// Last date.
    pub to: NaiveDate,
    /// Starting equity of each run.
    pub equity: Money,
    /// Adverse slippage in ticks.
    pub slippage_ticks: Decimal,
    /// Close time of a daily bar (UTC).
    pub close_time_utc: chrono::NaiveTime,
}

impl std::fmt::Debug for ValidationInput<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ValidationInput")
            .field("version", &self.info.reference.version_id)
            .field("from", &self.from)
            .field("to", &self.to)
            .finish_non_exhaustive()
    }
}

struct NoEvidence;

async fn run_range(
    input: &ValidationInput<'_>,
    research: &RiskConfig,
    costs: &dyn CostModel,
    start: NaiveDate,
    end: NaiveDate,
) -> Result<crate::engine::BacktestReport, BacktestError> {
    // Research runs simulate the version at the Paper stage whatever its
    // registry stage (ADR 0006); the Risk Gate refuses non-trading stages.
    let mut strategy = input.info.clone();
    strategy.stage = qd_domain::lifecycle::strategy::StrategyStage::Paper;
    let config = BacktestConfig {
        initial_equity: input.equity,
        start,
        end,
        strategy,
        evidence: EvidencePolicy::ResearchPrior,
        slippage_ticks: input.slippage_ticks,
        close_time_utc: input.close_time_utc,
        calendar_version: "validation".to_owned(),
        plan_slippage_ticks: Some(input.slippage_ticks),
    };
    run_backtest(
        input.strategy,
        input.instruments,
        &config,
        research,
        costs,
        &NoEvidence,
    )
    .await
}

impl EvidenceSource for NoEvidence {
    fn evidence(&self, _: StrategyVersionId, _: &str) -> Option<Evidence> {
        None
    }
}

fn check(name: &'static str, passed: bool, detail: String) -> Check {
    Check {
        name,
        passed,
        detail,
    }
}

/// Validates a strategy version. See the module docs for the protocol.
pub async fn validate(
    input: &ValidationInput<'_>,
    criteria: &ValidationCriteria,
    risk: &RiskConfig,
    costs: &dyn CostModel,
) -> Result<ValidationReport, BacktestError> {
    criteria.validate().map_err(BacktestError::Invalid)?;
    if input.to <= input.from {
        return Err(BacktestError::EmptyRange);
    }
    let research = research_risk_config(risk)
        .ok_or_else(|| BacktestError::Invalid("invalid research risk configuration".to_owned()))?;
    let span = (input.to - input.from).num_days();
    let dev_days = Decimal::from(span) * (Decimal::ONE - criteria.holdout_fraction);
    let dev_days = rust_decimal::prelude::ToPrimitive::to_i64(&dev_days.trunc()).unwrap_or(span);
    let holdout_from = input.from + Duration::days(dev_days.max(1));

    let mut windows = Vec::new();
    let mut oos_trades = Vec::new();
    let mut start = input.from;
    while start < holdout_from {
        let end = (start + Duration::days(criteria.window_days - 1))
            .min(holdout_from - Duration::days(1));
        match run_range(input, &research, costs, start, end).await {
            Ok(report) => {
                oos_trades.extend(report.trades.iter().cloned());
                windows.push(WindowResult {
                    start,
                    end,
                    metrics: report.metrics,
                    open_positions: report.open_positions,
                });
            }
            // A window without bars (a data gap) is simply empty.
            Err(BacktestError::EmptyRange) => {}
            Err(e) => return Err(e),
        }
        start = end + Duration::days(1);
    }
    let holdout = match run_range(input, &research, costs, holdout_from, input.to).await {
        Ok(report) => report,
        Err(BacktestError::EmptyRange) => {
            return Err(BacktestError::Invalid("the holdout has no bars".to_owned()));
        }
        Err(e) => return Err(e),
    };

    let initial = input.equity.amount;
    let oos = Metrics::compute(&oos_trades, &[], initial);
    let r_multiples: Vec<Decimal> = oos_trades.iter().map(|t| t.r_multiple).collect();
    let mc = monte_carlo(
        &r_multiples,
        risk.risk_per_trade.value(),
        risk.hard_halt_drawdown.value(),
        MonteCarloConfig {
            paths: criteria.monte_carlo_paths,
            seed: criteria.monte_carlo_seed,
        },
    );
    let traded: Vec<&WindowResult> = windows.iter().filter(|w| w.metrics.trades > 0).collect();
    let positive = traded
        .iter()
        .filter(|w| w.metrics.expectancy_r > Decimal::ZERO)
        .count();
    let positive_share = if traded.is_empty() {
        Decimal::ZERO
    } else {
        Decimal::from(u32::try_from(positive).unwrap_or(0))
            / Decimal::from(u32::try_from(traded.len()).unwrap_or(1))
    };
    let worst_window_dd = windows
        .iter()
        .map(|w| w.metrics.max_drawdown)
        .max()
        .unwrap_or(Decimal::ZERO);

    let mut checks = vec![
        check(
            "oos_trades",
            oos.trades >= criteria.min_oos_trades,
            format!(
                "{} out-of-sample trades, need {}",
                oos.trades, criteria.min_oos_trades
            ),
        ),
        check(
            "oos_expectancy",
            oos.trades > 0 && oos.expectancy_r >= criteria.min_oos_expectancy_r,
            format!(
                "{:.3}R mean, need {}R",
                oos.expectancy_r, criteria.min_oos_expectancy_r
            ),
        ),
        check(
            "window_consistency",
            positive_share >= criteria.min_positive_window_share,
            format!(
                "{positive} of {} windows with trades positive, need share {}",
                traded.len(),
                criteria.min_positive_window_share
            ),
        ),
        check(
            "window_drawdown",
            worst_window_dd <= criteria.max_window_drawdown,
            format!(
                "worst window drawdown {:.4}, limit {}",
                worst_window_dd, criteria.max_window_drawdown
            ),
        ),
        check(
            "holdout_trades",
            holdout.metrics.trades >= criteria.min_holdout_trades,
            format!(
                "{} holdout trades, need {}",
                holdout.metrics.trades, criteria.min_holdout_trades
            ),
        ),
        check(
            "holdout_expectancy",
            holdout.metrics.trades > 0
                && holdout.metrics.expectancy_r >= criteria.min_holdout_expectancy_r,
            format!(
                "{:.3}R mean, need {}R",
                holdout.metrics.expectancy_r, criteria.min_holdout_expectancy_r
            ),
        ),
    ];
    match &mc {
        Some(mc) => {
            checks.push(check(
                "monte_carlo_drawdown",
                mc.max_drawdown_p95 <= criteria.max_mc_drawdown_p95,
                format!(
                    "p95 drawdown {}, limit {}",
                    mc.max_drawdown_p95, criteria.max_mc_drawdown_p95
                ),
            ));
            checks.push(check(
                "monte_carlo_halt_risk",
                mc.prob_drawdown_over_limit <= criteria.max_mc_prob_over_halt,
                format!(
                    "{} of paths reach the hard-halt drawdown, limit {}",
                    mc.prob_drawdown_over_limit, criteria.max_mc_prob_over_halt
                ),
            ));
        }
        None => checks.push(check(
            "monte_carlo",
            false,
            "no out-of-sample trades to resample".to_owned(),
        )),
    }
    let passed = checks.iter().all(|c| c.passed);
    Ok(ValidationReport {
        version: input.info.reference.version_id,
        logic_version: input.info.reference.logic_version.clone(),
        instruments: input
            .instruments
            .iter()
            .map(|d| d.spec.symbol.clone())
            .collect(),
        from: input.from,
        holdout_from,
        to: input.to,
        criteria: criteria.clone(),
        windows,
        oos,
        holdout: holdout.metrics,
        monte_carlo: mc,
        evidence: evidence_tables(&oos_trades),
        oos_trades,
        checks,
        passed,
    })
}
