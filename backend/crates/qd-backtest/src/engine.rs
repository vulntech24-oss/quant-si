//! The backtest engine: runs the shared daily cycle
//! ([`qd_app::session::TradingSession`]) day by day on historical bars, with
//! the paper venue's fill rules, an in-memory journal and in-memory halts.
//! Paper trading runs exactly the same cycle (INV-08).
//!
//! Supported: account currency equal to the instrument currency (no FX
//! series yet). Costs come from the shared cost model and are charged when a
//! trade closes.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use chrono::{NaiveDate, NaiveTime, TimeZone, Utc};
use qd_app::decision::{DecisionEngine, EvidencePolicy, StrategyVersionInfo};
use qd_app::gateway::{GatewayAccount, OrderGateway};
use qd_app::live::LivePolicy;
use qd_app::memory::{InMemoryHaltStore, InMemoryJournal};
use qd_app::ports::{Clock, EvidenceSource};
use qd_app::session::{
    AccountBook, SessionClock, SessionParts, SessionSettings, StrategySlot, TradingSession,
};
use qd_domain::costs::CostModel;
use qd_domain::ids::{AccountId, SnapshotId};
use qd_domain::instrument::InstrumentSpec;
use qd_domain::market::Bar;
use qd_domain::num::Money;
use qd_domain::proposal::AccountMode;
use qd_risk::config::RiskConfig;
use qd_strategy::strategy::Strategy;
use rust_decimal::Decimal;
use serde::Serialize;
use thiserror::Error;

use crate::metrics::{Metrics, TradeRecord};
use crate::sim::SimBroker;

pub use qd_app::session::InstrumentData;

/// Backtest settings.
#[derive(Clone, Debug)]
pub struct BacktestConfig {
    /// Starting equity, in the account currency.
    pub initial_equity: Money,
    /// First decision date (earlier bars are warm-up history).
    pub start: NaiveDate,
    /// Last date processed.
    pub end: NaiveDate,
    /// Strategy version under test.
    pub strategy: StrategyVersionInfo,
    /// Evidence policy (research runs use the neutral prior).
    pub evidence: EvidencePolicy,
    /// Adverse slippage on market and stop fills, in ticks.
    pub slippage_ticks: Decimal,
    /// Time of day (UTC) at which each date's bar is considered complete.
    pub close_time_utc: NaiveTime,
    /// Trading-calendar version recorded with each decision.
    pub calendar_version: String,
}

/// Why a backtest could not run.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BacktestError {
    /// An instrument is quoted in another currency than the account.
    #[error("instrument {0} is not in the account currency; FX series are not supported yet")]
    CurrencyMismatch(String),
    /// The journal failed.
    #[error("journal failure: {0}")]
    Journal(String),
    /// The date range is empty.
    #[error("empty date range")]
    EmptyRange,
}

/// One point of the equity curve.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct EquityPoint {
    /// Date.
    pub date: NaiveDate,
    /// Equity at the close.
    pub equity: Decimal,
}

/// The result of a backtest.
#[derive(Clone, Debug, Serialize)]
pub struct BacktestReport {
    /// Closed trades.
    pub trades: Vec<TradeRecord>,
    /// Daily equity.
    pub equity_curve: Vec<EquityPoint>,
    /// Summary metrics.
    pub metrics: Metrics,
    /// Decisions by outcome code (`enter` or the NO TRADE reason code).
    pub decisions: HashMap<String, u32>,
    /// Positions still open at the end (marked to market in the curve).
    pub open_positions: usize,
    /// Days on which a position was left without protection.
    pub unprotected_events: u32,
    /// Reconciliation mismatches between the book and the simulated broker.
    pub reconciliation_mismatches: u32,
}

/// Runs one backtest.
pub async fn run_backtest(
    strategy: &dyn Strategy,
    instruments: &[InstrumentData],
    config: &BacktestConfig,
    risk: &RiskConfig,
    costs: &dyn CostModel,
    evidence: &dyn EvidenceSource,
) -> Result<BacktestReport, BacktestError> {
    let currency = config.initial_equity.currency;
    for data in instruments {
        if data.spec.currency != currency {
            return Err(BacktestError::CurrencyMismatch(data.spec.symbol.clone()));
        }
    }
    let at = |date: NaiveDate| Utc.from_utc_datetime(&date.and_time(config.close_time_utc));
    let dates: BTreeSet<NaiveDate> = instruments
        .iter()
        .flat_map(|d| d.series.bars().iter().map(Bar::date))
        .filter(|d| *d >= config.start && *d <= config.end)
        .collect();
    let first = *dates.iter().next().ok_or(BacktestError::EmptyRange)?;

    let clock = Arc::new(SessionClock::new(at(first)));
    let specs: Vec<InstrumentSpec> = instruments.iter().map(|d| d.spec.clone()).collect();
    let broker = Arc::new(SimBroker::new(
        clock.clone(),
        specs.clone(),
        config.slippage_ticks,
    ));
    let journal = Arc::new(InMemoryJournal::new());
    let halts = Arc::new(InMemoryHaltStore::new());
    let account_id = AccountId::new_at(at(first));
    let gateway = Arc::new(OrderGateway::new(
        GatewayAccount {
            id: account_id,
            mode: AccountMode::Backtest,
            live_armed: false,
        },
        broker.clone(),
        journal.clone(),
        halts.clone(),
        clock.clone(),
        LivePolicy::default(),
        specs,
    ));
    let mut session = TradingSession::new(SessionParts {
        gateway,
        venue: broker,
        journal,
        halts,
        market_clock: clock.clone(),
        wall_clock: clock.clone() as Arc<dyn Clock>,
        book: AccountBook::new(account_id, config.initial_equity, first),
        settings: SessionSettings {
            mode: AccountMode::Backtest,
            close_time_utc: config.close_time_utc,
            calendar_version: config.calendar_version.clone(),
            snapshot: SnapshotId::new_at(at(first)),
        },
    });
    let engine = DecisionEngine::new(risk, costs, evidence, config.evidence);
    let slots = [StrategySlot {
        strategy,
        info: config.strategy.clone(),
        slippage_ticks: None,
    }];
    let initial = config.initial_equity.amount;
    let mut report = BacktestReport {
        trades: Vec::new(),
        equity_curve: Vec::new(),
        metrics: Metrics::default(),
        decisions: HashMap::new(),
        open_positions: 0,
        unprotected_events: 0,
        reconciliation_mismatches: 0,
    };
    for date in dates {
        let day = session
            .process_day(date, instruments, &slots, &engine, risk, costs)
            .await
            .map_err(|e| BacktestError::Journal(e.to_string()))?;
        report.trades.extend(day.trades);
        report.equity_curve.push(EquityPoint {
            date,
            equity: day.equity,
        });
        for (code, n) in day.decisions {
            *report.decisions.entry(code).or_default() += n;
        }
        report.unprotected_events += day.unprotected;
        report.reconciliation_mismatches += day.mismatches;
    }
    report.open_positions = session
        .positions()
        .active()
        .iter()
        .filter(|p| !p.quantity.is_zero())
        .count();
    report.metrics = Metrics::compute(&report.trades, &report.equity_curve, initial);
    Ok(report)
}

/// A research copy of a risk configuration: identical limits, but the minimum
/// EV is zero, because research runs have no evidence yet (ADR 0006).
#[must_use]
pub fn research_risk_config(base: &RiskConfig) -> Option<RiskConfig> {
    let mut data = qd_risk::config::RiskConfigData::clone(base);
    data.min_ev_r = Decimal::ZERO;
    RiskConfig::new(data).ok()
}
