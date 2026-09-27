//! The backtest engine: runs the shared use cases day by day on historical bars (INV-08).
//!
//! Each trading date, per instrument with a bar:
//!
//! 1. The simulated broker processes the bar; fills go through the Order
//!    Gateway to the Position Manager, exactly as in paper and live.
//! 2. Protective stops placed on the entry bar are checked against that bar.
//! 3. The Position Manager applies the time exit and invalidation rules.
//!
//! Then equity is marked to the close, halt triggers run, and every
//! instrument without an active position is evaluated: strategy → Decision
//! Engine (journaled) → Position Manager → Order Gateway.
//!
//! Supported: account currency equal to the instrument currency (no FX
//! series yet). Costs come from the shared cost model and are charged when a
//! trade closes.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use chrono::{DateTime, Datelike, NaiveDate, NaiveTime, TimeZone, Utc};
use qd_app::decision::{DecisionContext, DecisionEngine, EvidencePolicy, StrategyVersionInfo};
use qd_app::gateway::{GatewayAccount, OrderGateway};
use qd_app::live::LivePolicy;
use qd_app::memory::{InMemoryHaltStore, InMemoryJournal};
use qd_app::ports::{EvidenceSource, HaltStore, Journal};
use qd_app::positions::{Position, PositionManager, PositionUpdate};
use qd_domain::action::Side;
use qd_domain::costs::{CostModel, CostRequest};
use qd_domain::halt::HaltState;
use qd_domain::ids::{AccountId, DecisionId, HaltId, InstrumentId, SnapshotId};
use qd_domain::instrument::{InstrumentSpec, ProductType};
use qd_domain::lifecycle::position::PositionState;
use qd_domain::market::{Bar, BarSeries};
use qd_domain::num::{Currency, FxRate, Money, Price};
use qd_domain::outcome::{DecisionOutcome, ExitReason};
use qd_domain::portfolio::{PositionExposure, Protection, position_open_risk};
use qd_domain::proposal::AccountMode;
use qd_risk::config::RiskConfig;
use qd_risk::gate::{AccountRiskState, RiskItem};
use qd_risk::triggers::halt_triggers;
use qd_strategy::regime::RegimeClassifier;
use qd_strategy::strategy::{Strategy, run_strategy};
use rust_decimal::Decimal;
use serde::Serialize;
use thiserror::Error;

use crate::metrics::{Metrics, TradeRecord};
use crate::sim::{BarEvents, SimBroker, SimClock};

/// One instrument's data for a backtest.
#[derive(Clone, Debug)]
pub struct InstrumentData {
    /// Spec.
    pub spec: InstrumentSpec,
    /// Product to trade.
    pub product: ProductType,
    /// All completed bars.
    pub series: BarSeries,
}

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

struct Account {
    id: AccountId,
    currency: Currency,
    initial: Decimal,
    realized_net: Decimal,
    high_water_mark: Decimal,
    day_start: Decimal,
    week_start: Decimal,
    week: (i32, u32),
    consecutive_losses: u32,
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

    let clock = Arc::new(SimClock::new(at(first)));
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
    let positions = PositionManager::new(gateway.clone(), journal.clone(), clock.clone());
    let engine = DecisionEngine::new(risk, costs, evidence, config.evidence);
    let snapshot = SnapshotId::new_at(at(first));
    let initial = config.initial_equity.amount;
    let mut account = Account {
        id: account_id,
        currency,
        initial,
        realized_net: Decimal::ZERO,
        high_water_mark: initial,
        day_start: initial,
        week_start: initial,
        week: (first.iso_week().year(), first.iso_week().week()),
        consecutive_losses: 0,
    };
    let by_id: HashMap<InstrumentId, &InstrumentData> =
        instruments.iter().map(|d| (d.spec.id, d)).collect();
    let mut closes: HashMap<InstrumentId, Price> = HashMap::new();
    let mut report = BacktestReport {
        trades: Vec::new(),
        equity_curve: Vec::new(),
        metrics: Metrics::default(),
        decisions: HashMap::new(),
        open_positions: 0,
        unprotected_events: 0,
        reconciliation_mismatches: 0,
    };
    let mut recorded: BTreeSet<qd_domain::ids::PositionId> = BTreeSet::new();

    for date in dates {
        clock.set(at(date));
        for data in instruments {
            let Some(bar) = data
                .series
                .bars()
                .iter()
                .find(|b| b.date() == date)
                .copied()
            else {
                continue;
            };
            closes.insert(data.spec.id, bar.close());
            let events = broker.process_bar(data.spec.id, &bar);
            apply_events(&gateway, &positions, events, date, &mut report).await;
            let same_bar = broker.process_same_bar_protection(data.spec.id, &bar);
            apply_events(&gateway, &positions, same_bar, date, &mut report).await;
            let update = positions.on_bar_close(data.spec.id, &bar).await;
            count_update(&update, &mut report);
        }

        // Close out trades, charge costs, mark to market.
        for p in positions.positions() {
            if p.state == PositionState::Closed
                && !recorded.contains(&p.id)
                && !p.entered_quantity.is_zero()
            {
                recorded.insert(p.id);
                if let Some(data) = by_id.get(&p.instrument) {
                    let trade = close_trade(&p, data, costs, date)?;
                    account.realized_net += trade.net_pnl;
                    account.consecutive_losses = if trade.net_pnl < Decimal::ZERO {
                        account.consecutive_losses + 1
                    } else {
                        0
                    };
                    report.trades.push(trade);
                }
            }
        }
        let unrealized: Decimal = positions
            .active()
            .iter()
            .map(|p| unrealized(p, closes.get(&p.instrument).copied()))
            .sum();
        let equity = account.initial + account.realized_net + unrealized;
        report.equity_curve.push(EquityPoint { date, equity });
        let week = (date.iso_week().year(), date.iso_week().week());
        if week != account.week {
            account.week = week;
            account.week_start = equity;
        }
        let state = risk_state(
            &account,
            equity,
            &positions,
            &closes,
            &by_id,
            risk,
            halts.as_ref(),
        )
        .await;
        if let Ok(proposals) = halt_triggers(&state, risk, at(date)) {
            for proposal in proposals {
                if let Ok(halt) = proposal.into_halt(HaltId::new_at(at(date)), at(date)) {
                    let _ = halts.record(&halt).await;
                }
            }
        }
        if let Ok(mismatches) = broker_reconcile(&positions, broker.as_ref()).await {
            report.reconciliation_mismatches += mismatches;
        }

        // Decisions at the close, for instruments without an active position.
        let state = risk_state(
            &account,
            equity,
            &positions,
            &closes,
            &by_id,
            risk,
            halts.as_ref(),
        )
        .await;
        for data in instruments {
            if data.series.last().is_none() || !data.series.bars().iter().any(|b| b.date() == date)
            {
                continue;
            }
            let series = data.series.as_of(date);
            let Ok(evaluation) = run_strategy(strategy, &data.spec, &series, &RegimeClassifier::V1)
            else {
                continue;
            };
            let ctx = DecisionContext {
                decision_id: DecisionId::new_at(at(date)),
                at: at(date),
                expected_last_completed: date,
                spec: &data.spec,
                product: data.product,
                fx: FxRate::identity(currency, at(date)),
                strategy: &config.strategy,
                evaluation: &evaluation,
                series: &series,
                account: &state,
                already_in_position: positions.has_active(data.spec.id),
                snapshot_id: snapshot,
                calendar_version: &config.calendar_version,
            };
            let decided = engine
                .decide_and_journal(&ctx, journal.as_ref() as &dyn Journal)
                .await
                .map_err(|e| BacktestError::Journal(e.to_string()))?;
            let Some(decided) = decided else { continue };
            let code = match &decided.record.outcome {
                DecisionOutcome::NoTrade { reason } => reason.code().to_owned(),
                other => other.headline().to_owned(),
            };
            *report.decisions.entry(code).or_default() += 1;
            if let (Some(authorization), Some(proposal)) =
                (decided.authorization, decided.record.proposal.as_ref())
            {
                // A refused entry is already journaled by the gateway.
                let _ = positions
                    .open(
                        &authorization,
                        proposal,
                        &data.spec,
                        config.strategy.stage,
                        data.product,
                    )
                    .await;
            }
        }
        account.day_start = equity;
        account.high_water_mark = account.high_water_mark.max(equity);
    }
    report.open_positions = positions
        .active()
        .iter()
        .filter(|p| !p.quantity.is_zero())
        .count();
    report.metrics = Metrics::compute(&report.trades, &report.equity_curve, initial);
    Ok(report)
}

async fn apply_events(
    gateway: &OrderGateway,
    positions: &PositionManager,
    events: BarEvents,
    date: NaiveDate,
    report: &mut BacktestReport,
) {
    for fill in events.fills {
        if let Ok(fill_report) = gateway.on_fill(fill).await {
            let update = positions.on_fill(&fill_report, date).await;
            count_update(&update, report);
        }
    }
    for expired in events.expired {
        if gateway.on_expired(expired).await.is_ok() {
            positions.on_entry_ended(expired).await;
        }
    }
}

fn count_update(update: &PositionUpdate, report: &mut BacktestReport) {
    report.unprotected_events += u32::try_from(update.unprotected.len()).unwrap_or(u32::MAX);
}

fn unrealized(p: &Position, mark: Option<Price>) -> Decimal {
    let (Some(entry), Some(mark)) = (p.entry_price, mark) else {
        return Decimal::ZERO;
    };
    let per_unit = match p.side {
        Side::Long => mark.value() - entry.value(),
        Side::Short => entry.value() - mark.value(),
    };
    per_unit * p.quantity.value() * p.multiplier
}

fn close_trade(
    p: &Position,
    data: &InstrumentData,
    costs: &dyn CostModel,
    date: NaiveDate,
) -> Result<TradeRecord, BacktestError> {
    let entry = p.entry_price.map_or(Decimal::ZERO, Price::value);
    let units = p.entered_quantity.value() * p.multiplier;
    let exit_per_unit = if units.is_zero() {
        Decimal::ZERO
    } else {
        p.realized_gross / units
    };
    let exit = match p.side {
        Side::Long => entry + exit_per_unit,
        Side::Short => entry - exit_per_unit,
    };
    let cost = match (Price::new(entry), Price::new(exit)) {
        (Ok(e), Ok(x)) => costs
            .quote(&CostRequest {
                spec: &data.spec,
                product: data.product,
                side: p.side,
                quantity: p.entered_quantity,
                entry: e,
                exit: x,
                trade_date: date,
            })
            .map(|q| q.estimate.total())
            .unwrap_or(Decimal::ZERO),
        _ => Decimal::ZERO,
    };
    let net = p.realized_gross - cost;
    let planned = p.risk_net_per_unit * p.entered_quantity.value();
    let r = if planned.is_zero() {
        Decimal::ZERO
    } else {
        net / planned
    };
    Ok(TradeRecord {
        instrument: data.spec.symbol.clone(),
        side: p.side,
        opened_on: p.opened_on.unwrap_or(date),
        closed_on: date,
        quantity: p.entered_quantity,
        entry_price: entry,
        exit_price: exit,
        gross_pnl: p.realized_gross,
        costs: cost,
        net_pnl: net,
        r_multiple: r,
        exit_reason: p.exit_reason.unwrap_or(ExitReason::Manual),
    })
}

#[allow(clippy::too_many_arguments)] // assembling the Risk Gate's view needs each input
async fn risk_state(
    account: &Account,
    equity: Decimal,
    positions: &PositionManager,
    closes: &HashMap<InstrumentId, Price>,
    by_id: &HashMap<InstrumentId, &InstrumentData>,
    risk: &RiskConfig,
    halts: &dyn HaltStore,
) -> AccountRiskState {
    let money = |amount: Decimal| Money::new(amount, account.currency);
    let at: DateTime<Utc> = positions.now();
    let mut open_risk = Vec::new();
    for p in positions.active() {
        let Some(data) = by_id.get(&p.instrument) else {
            continue;
        };
        let fx = FxRate::identity(account.currency, at);
        let amount = if p.state == PositionState::Opening {
            money(p.risk_net_per_unit * p.planned_quantity.value())
        } else {
            let Some(mark) = closes.get(&p.instrument).copied() else {
                continue;
            };
            let protection = if p.state == PositionState::Protected {
                Protection::Valid { stop: p.stop }
            } else {
                Protection::Missing
            };
            match position_open_risk(&PositionExposure {
                side: p.side,
                quantity: p.quantity,
                mark,
                protection,
                multiplier: p.multiplier,
                fx,
                gap_shock: risk.gap_shocks.for_asset_class(data.spec.asset_class),
            }) {
                Ok(r) => r.amount,
                Err(_) => continue,
            }
        };
        open_risk.push(RiskItem {
            instrument: p.instrument,
            strategy_version: p.strategy_version,
            bucket: data.spec.correlation_bucket.clone(),
            amount,
        });
    }
    let halt_state = match halts.load().await {
        Ok(h) => HaltState::Known(h),
        Err(_) => HaltState::Unknown,
    };
    AccountRiskState {
        account: account.id,
        mode: AccountMode::Backtest,
        equity: money(equity),
        equity_at_day_start: money(account.day_start),
        equity_at_week_start: money(account.week_start),
        high_water_mark: money(account.high_water_mark.max(equity)),
        consecutive_losses: account.consecutive_losses,
        open_risk,
        halts: halt_state,
    }
}

async fn broker_reconcile(positions: &PositionManager, broker: &SimBroker) -> Result<u32, ()> {
    use qd_app::ports::BrokerAccountReader;
    let broker_positions = broker.positions().await.map_err(|_| ())?;
    Ok(u32::try_from(positions.reconcile(&broker_positions).len()).unwrap_or(u32::MAX))
}

/// A research copy of a risk configuration: identical limits, but the minimum
/// EV is zero, because research runs have no evidence yet (ADR 0006).
#[must_use]
pub fn research_risk_config(base: &RiskConfig) -> Option<RiskConfig> {
    let mut data = qd_risk::config::RiskConfigData::clone(base);
    data.min_ev_r = Decimal::ZERO;
    RiskConfig::new(data).ok()
}
