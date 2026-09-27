//! Walk-forward parameter search (ADR 0015). Research only.
//!
//! A small, fixed grid of parameter sets around each strategy's catalog
//! version is evaluated with an honest walk-forward:
//!
//! 1. Split `[from, to]` into consecutive test windows of `test_days`.
//! 2. For each test window, run every candidate over the `train_days`
//!    before it and **select** the one with the best mean R (at least
//!    `min_train_trades` trades; otherwise the catalog version).
//! 3. Run only the selected candidate over the test window. Nothing in the
//!    test window influenced the choice.
//! 4. The out-of-sample result of the whole procedure is the concatenation
//!    of the test windows. It is compared with the in-sample result of the
//!    chosen candidates (the gap measures overfitting) and with the catalog
//!    version run over the same test windows (the baseline).
//!
//! The report is not evidence and cannot be registered: a registered
//! version runs only with its catalog parameters (INV-10). A parameter set
//! worth trading becomes a new logic version in code, then goes through
//! validation and paper trading like any other.

use std::collections::BTreeMap;

use chrono::{Duration, NaiveDate};
use qd_app::decision::{EvidencePolicy, StrategyVersionInfo};
use qd_app::ports::{Evidence, EvidenceSource};
use qd_domain::costs::CostModel;
use qd_domain::ids::StrategyVersionId;
use qd_domain::lifecycle::strategy::StrategyStage;
use qd_domain::num::Money;
use qd_risk::config::RiskConfig;
use qd_strategy::breakout::{Breakout, BreakoutParams};
use qd_strategy::mean_reversion::{MeanReversion, MeanReversionParams};
use qd_strategy::strategy::Strategy;
use qd_strategy::trend_pullback::{TrendPullback, TrendPullbackParams};
use qd_strategy::trend_pullback_short::{TrendPullbackShort, TrendPullbackShortParams};
use rust_decimal::Decimal;
use serde::Serialize;
use serde_json::Value;

use crate::engine::{
    BacktestConfig, BacktestError, InstrumentData, research_risk_config, run_backtest,
};
use crate::metrics::{Metrics, TradeRecord};

/// Most candidates in a grid: a small grid keeps the multiple-testing
/// problem small.
pub const MAX_CANDIDATES: usize = 12;

/// One parameter set.
pub struct Candidate {
    /// The parameters, as JSON.
    pub parameters: Value,
    /// The strategy with them.
    pub strategy: Box<dyn Strategy>,
    /// Its RR floor.
    pub rr_floor: Decimal,
    /// Whether these are the catalog parameters.
    pub is_catalog: bool,
}

impl std::fmt::Debug for Candidate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Candidate")
            .field("parameters", &self.parameters)
            .finish_non_exhaustive()
    }
}

fn candidate<S, P>(strategy: S, params: P, catalog: P) -> Option<Candidate>
where
    S: Strategy + 'static,
    P: Serialize + PartialEq,
{
    let parameters = serde_json::to_value(&params).ok()?;
    let rr_floor = parameters.get("rr_floor")?.as_str()?.parse().ok()?;
    Some(Candidate {
        is_catalog: params == catalog,
        parameters,
        strategy: Box::new(strategy),
        rr_floor,
    })
}

fn d(value: i64, scale: u32) -> Decimal {
    Decimal::new(value, scale)
}

/// The grid for a logic version; `None` if the build has no grid for it.
#[must_use]
pub fn grid(logic_version: &str) -> Option<Vec<Candidate>> {
    let mut out = Vec::new();
    match logic_version {
        TrendPullback::LOGIC_VERSION => {
            for band in [d(25, 2), d(5, 1), d(1, 0)] {
                for stop in [d(15, 1), d(2, 0)] {
                    for target in [d(2, 0), d(25, 1)] {
                        let p = TrendPullbackParams {
                            pullback_band_atr: band,
                            stop_atr: stop,
                            target_r: target,
                            ..TrendPullbackParams::V1
                        };
                        out.push(candidate(
                            TrendPullback::with_params(p),
                            p,
                            TrendPullbackParams::V1,
                        )?);
                    }
                }
            }
        }
        TrendPullbackShort::LOGIC_VERSION => {
            for band in [d(25, 2), d(5, 1), d(1, 0)] {
                for stop in [d(15, 1), d(2, 0)] {
                    for target in [d(2, 0), d(25, 1)] {
                        let p = TrendPullbackShortParams {
                            pullback_band_atr: band,
                            stop_atr: stop,
                            target_r: target,
                            ..TrendPullbackShortParams::V1
                        };
                        out.push(candidate(
                            TrendPullbackShort::with_params(p),
                            p,
                            TrendPullbackShortParams::V1,
                        )?);
                    }
                }
            }
        }
        Breakout::LOGIC_VERSION => {
            for extension in [d(5, 1), d(1, 0)] {
                for stop in [d(15, 1), d(2, 0), d(25, 1)] {
                    for target in [d(25, 1), d(3, 0)] {
                        let p = BreakoutParams {
                            max_extension_atr: extension,
                            stop_atr: stop,
                            target_r: target,
                            ..BreakoutParams::V1
                        };
                        out.push(candidate(Breakout::with_params(p), p, BreakoutParams::V1)?);
                    }
                }
            }
        }
        MeanReversion::LOGIC_VERSION => {
            for stretch in [d(1, 0), d(15, 1), d(2, 0)] {
                for stop in [d(75, 2), d(1, 0), d(15, 1)] {
                    let p = MeanReversionParams {
                        stretch_atr: stretch,
                        stop_atr: stop,
                        ..MeanReversionParams::V1
                    };
                    out.push(candidate(
                        MeanReversion::with_params(p),
                        p,
                        MeanReversionParams::V1,
                    )?);
                }
            }
        }
        _ => return None,
    }
    out.truncate(MAX_CANDIDATES);
    Some(out)
}

/// Search settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct SearchSettings {
    /// Training period before each test window, calendar days.
    pub train_days: i64,
    /// Test window length, calendar days.
    pub test_days: i64,
    /// Fewer training trades than this and a candidate cannot be chosen.
    pub min_train_trades: u32,
}

impl Default for SearchSettings {
    fn default() -> Self {
        Self {
            train_days: 365,
            test_days: 91,
            min_train_trades: 10,
        }
    }
}

/// What happened in one test window.
#[derive(Clone, Debug, Serialize)]
pub struct SearchWindow {
    /// Training start.
    pub train_from: NaiveDate,
    /// Test start.
    pub test_from: NaiveDate,
    /// Test end.
    pub test_to: NaiveDate,
    /// Index of the chosen candidate.
    pub chosen: usize,
    /// Its training mean R.
    pub train_expectancy_r: Decimal,
    /// Its training trades.
    pub train_trades: u32,
    /// Its test mean R.
    pub test_expectancy_r: Decimal,
    /// Its test trades.
    pub test_trades: u32,
}

/// The search report.
#[derive(Clone, Debug, Serialize)]
pub struct SearchReport {
    /// Logic version searched.
    pub logic_version: String,
    /// Settings.
    pub settings: SearchSettings,
    /// Candidates, by index.
    pub candidates: Vec<Value>,
    /// Test windows in order.
    pub windows: Vec<SearchWindow>,
    /// How often each candidate was chosen.
    pub chosen_counts: BTreeMap<usize, u32>,
    /// Out-of-sample result of the whole procedure.
    pub out_of_sample: Metrics,
    /// Mean training R of the chosen candidates (what the search "saw").
    pub mean_in_sample_r: Decimal,
    /// The catalog parameters over the same test windows.
    pub baseline: Metrics,
    /// Candidates tried per window (for multiple-testing awareness).
    pub trials_per_window: usize,
    /// Plain-language reading of the result.
    pub verdict: String,
}

struct NoEvidence;

impl EvidenceSource for NoEvidence {
    fn evidence(&self, _: StrategyVersionId, _: &str) -> Option<Evidence> {
        None
    }
}

/// What a search runs on.
#[derive(Clone, Copy)]
pub struct SearchInput<'a> {
    /// The version's identity (name, logic version); parameters come from the grid.
    pub info: &'a StrategyVersionInfo,
    /// Instruments with their bars.
    pub instruments: &'a [InstrumentData],
    /// First test date (training reaches back before it).
    pub from: NaiveDate,
    /// Last date.
    pub to: NaiveDate,
    /// Starting equity of each run.
    pub equity: Money,
    /// Adverse slippage in ticks.
    pub slippage_ticks: Decimal,
}

async fn run(
    input: &SearchInput<'_>,
    c: &Candidate,
    research: &RiskConfig,
    costs: &dyn CostModel,
    start: NaiveDate,
    end: NaiveDate,
) -> Result<Vec<TradeRecord>, BacktestError> {
    let mut info = input.info.clone();
    info.stage = StrategyStage::Paper;
    info.rr_floor = c.rr_floor;
    let config = BacktestConfig {
        initial_equity: input.equity,
        start,
        end,
        strategy: info,
        evidence: EvidencePolicy::ResearchPrior,
        slippage_ticks: input.slippage_ticks,
        close_time_utc: chrono::NaiveTime::from_hms_opt(10, 0, 0).unwrap_or(chrono::NaiveTime::MIN),
        calendar_version: "search".to_owned(),
        plan_slippage_ticks: Some(input.slippage_ticks),
    };
    match run_backtest(
        c.strategy.as_ref(),
        input.instruments,
        &config,
        research,
        costs,
        &NoEvidence,
    )
    .await
    {
        Ok(report) => Ok(report.trades),
        Err(BacktestError::EmptyRange) => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

fn mean_r(trades: &[TradeRecord]) -> Decimal {
    if trades.is_empty() {
        Decimal::ZERO
    } else {
        trades.iter().map(|t| t.r_multiple).sum::<Decimal>()
            / Decimal::from(u32::try_from(trades.len()).unwrap_or(u32::MAX))
    }
}

/// Runs the search. See the module docs.
pub async fn walk_forward_search(
    input: &SearchInput<'_>,
    candidates: &[Candidate],
    settings: SearchSettings,
    risk: &RiskConfig,
    costs: &dyn CostModel,
) -> Result<SearchReport, BacktestError> {
    if candidates.is_empty() || candidates.len() > MAX_CANDIDATES {
        return Err(BacktestError::Invalid(format!(
            "a search needs 1 to {MAX_CANDIDATES} candidates"
        )));
    }
    if settings.train_days < 90 || settings.test_days < 20 || input.to <= input.from {
        return Err(BacktestError::Invalid(
            "train_days ≥ 90, test_days ≥ 20 and a non-empty range are required".to_owned(),
        ));
    }
    let research = research_risk_config(risk)
        .ok_or_else(|| BacktestError::Invalid("invalid research risk configuration".to_owned()))?;
    let catalog = candidates.iter().position(|c| c.is_catalog).unwrap_or(0);
    let mut windows = Vec::new();
    let mut oos = Vec::new();
    let mut baseline = Vec::new();
    let mut in_sample = Vec::new();
    let mut chosen_counts = BTreeMap::new();
    let mut test_from = input.from;
    while test_from <= input.to {
        let test_to = (test_from + Duration::days(settings.test_days - 1)).min(input.to);
        let train_from = test_from - Duration::days(settings.train_days);
        let train_to = test_from - Duration::days(1);
        // Selection sees only the training period.
        let mut best: Option<(usize, Decimal, u32)> = None;
        for (i, c) in candidates.iter().enumerate() {
            let trades = run(input, c, &research, costs, train_from, train_to).await?;
            let n = u32::try_from(trades.len()).unwrap_or(u32::MAX);
            if n < settings.min_train_trades {
                continue;
            }
            let r = mean_r(&trades);
            if best.is_none_or(|(_, b, _)| r > b) {
                best = Some((i, r, n));
            }
        }
        let (chosen, train_r, train_n) = best.unwrap_or((catalog, Decimal::ZERO, 0));
        let test = run(
            input,
            &candidates[chosen],
            &research,
            costs,
            test_from,
            test_to,
        )
        .await?;
        let base = run(
            input,
            &candidates[catalog],
            &research,
            costs,
            test_from,
            test_to,
        )
        .await?;
        *chosen_counts.entry(chosen).or_insert(0) += 1;
        in_sample.push(train_r);
        windows.push(SearchWindow {
            train_from,
            test_from,
            test_to,
            chosen,
            train_expectancy_r: train_r.round_dp(4),
            train_trades: train_n,
            test_expectancy_r: mean_r(&test).round_dp(4),
            test_trades: u32::try_from(test.len()).unwrap_or(u32::MAX),
        });
        oos.extend(test);
        baseline.extend(base);
        test_from = test_to + Duration::days(1);
    }
    let initial = input.equity.amount;
    let out_of_sample = Metrics::compute(&oos, &[], initial);
    let baseline = Metrics::compute(&baseline, &[], initial);
    let mean_in_sample_r = if in_sample.is_empty() {
        Decimal::ZERO
    } else {
        (in_sample.iter().copied().sum::<Decimal>()
            / Decimal::from(u32::try_from(in_sample.len()).unwrap_or(1)))
        .round_dp(4)
    };
    let verdict = if out_of_sample.trades < 30 {
        format!(
            "Only {} out-of-sample trades: too few to conclude anything.",
            out_of_sample.trades
        )
    } else if out_of_sample.expectancy_r <= baseline.expectancy_r {
        "The search did not beat the catalog parameters out of sample: keep them.".to_owned()
    } else if mean_in_sample_r > Decimal::ZERO
        && out_of_sample.expectancy_r < mean_in_sample_r / Decimal::TWO
    {
        "The search beat the baseline, but its out-of-sample result is less than half of what it saw in training: likely overfitting.".to_owned()
    } else {
        "The search beat the baseline out of sample. Treat this as a hypothesis: a new logic version still needs full validation and paper trading.".to_owned()
    };
    Ok(SearchReport {
        logic_version: input.info.reference.logic_version.clone(),
        settings,
        candidates: candidates.iter().map(|c| c.parameters.clone()).collect(),
        windows,
        chosen_counts,
        out_of_sample,
        mean_in_sample_r,
        baseline,
        trials_per_window: candidates.len(),
        verdict,
    })
}
