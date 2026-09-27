//! The paper-trading runner (ADR 0009).
//!
//! One run processes every trading day after the last processed one, in
//! order, with the shared daily cycle ([`TradingSession`], INV-08):
//!
//! 1. Take the cross-process run lock, so a day is never processed twice at once.
//! 2. Check that the account exists and is a paper account.
//! 3. Rebuild the book from the journal and check it. An inconsistent book
//!    records an operational halt that needs a human, and the run stops.
//! 4. Restore the paper venue (working orders, net positions) and the
//!    gateway, and resolve intents whose outcome was unknown.
//! 5. Evaluate every registered version at the Paper stage whose logic
//!    version and parameters match the code in this binary (INV-10).
//!    Paper decisions need validated evidence (INV-06): the evidence tables
//!    of each version's latest passed validation (ADR 0010). Without them,
//!    decisions are NO TRADE with `insufficient_evidence`.
//! 6. Process each date with bars and journal a `day_closed` entry.
//!
//! Halts and decisions use the wall clock, so a halt set today also blocks a
//! catch-up over earlier dates. Bars are read as known at the start of the run (INV-09).

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{Duration, NaiveDate, NaiveTime, TimeZone, Utc};
use qd_app::decision::{DecisionEngine, EvidencePolicy, StrategyVersionInfo};
use qd_app::gateway::{GatewayAccount, OrderGateway};
use qd_app::live::LivePolicy;
use qd_app::ports::{
    AccountStore, Clock, Evidence, EvidenceSource, HaltStore, HistoricalMarketData, Journal,
    JournalReader, PaperTrading, Reconciler, RunLock, StoreError,
};
use qd_app::positions::reconcile_book;
use qd_app::registry::StrategyRegistry;
use qd_app::restore::{RestoreError, RestoredState, STATE_KINDS};
use qd_app::session::{
    AccountBook, DayRecord, InstrumentData, SessionClock, SessionError, SessionParts,
    SessionSettings, StrategySlot, TradingSession,
};
use qd_domain::costs::CostModel;
use qd_domain::economics::SlippageAssumption;
use qd_domain::halt::{Halt, HaltKind, HaltScope};
use qd_domain::ids::{AccountId, HaltId, InstrumentId, SnapshotId, StrategyVersionId};
use qd_domain::instrument::{InstrumentKind, InstrumentSpec, ProductType};
use qd_domain::lifecycle::order::OrderIntentState;
use qd_domain::lifecycle::strategy::StrategyStage;
use qd_domain::market::{Bar, BarSeries};
use qd_domain::num::{Currency, Money};
use qd_domain::proposal::AccountMode;
use qd_risk::config::RiskConfig;
use rust_decimal::Decimal;
use serde::Serialize;
use serde_json::{Value, json};
use thiserror::Error;

use crate::venue::PaperBroker;

/// A source with no evidence: every decision fails the evidence gate.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoEvidenceTables;

impl EvidenceSource for NoEvidenceTables {
    fn evidence(&self, _: StrategyVersionId, _: &str) -> Option<Evidence> {
        None
    }
}

/// Paper-trading settings (from the server configuration).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaperSettings {
    /// The paper account.
    pub account: AccountId,
    /// Starting equity, account currency.
    pub initial_equity: Decimal,
    /// First date to process.
    pub start: NaiveDate,
    /// Time of day (UTC) at which a date's bar counts as complete.
    pub close_time_utc: NaiveTime,
    /// Adverse slippage on market and stop fills, in ticks.
    pub slippage_ticks: Decimal,
    /// Calendar-days of history loaded before the first date for indicators.
    pub warm_up_days: i64,
    /// Trading-calendar version recorded with decisions.
    pub calendar_version: String,
}

/// What the runner uses.
#[derive(Clone)]
pub struct PaperDeps {
    /// Journal (writes).
    pub journal: Arc<dyn Journal>,
    /// Journal (reads, for restore).
    pub reader: Arc<dyn JournalReader>,
    /// Kill switch.
    pub halts: Arc<dyn HaltStore>,
    /// Instruments and bars.
    pub market: Arc<dyn HistoricalMarketData>,
    /// Strategy registry.
    pub registry: StrategyRegistry,
    /// Accounts.
    pub accounts: Arc<dyn AccountStore>,
    /// Cost model.
    pub costs: Arc<dyn CostModel>,
    /// Risk configuration.
    pub risk: RiskConfig,
    /// Evidence tables, loaded at the start of each run.
    pub evidence: Arc<dyn qd_app::evidence::EvidenceLoader>,
    /// Run lock.
    pub lock: Arc<dyn RunLock>,
    /// Wall clock.
    pub clock: Arc<dyn Clock>,
}

/// Why a paper run failed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PaperError {
    /// Another run holds the lock.
    #[error("another paper run is in progress")]
    Busy,
    /// The configured account does not exist.
    #[error("the paper account does not exist")]
    UnknownAccount,
    /// The configured account is not a paper account.
    #[error("the configured account is not a paper account")]
    NotPaperAccount,
    /// The journal state could not be restored; an operational halt was recorded.
    #[error("restore failed: {0}")]
    Restore(RestoreError),
    /// A position or order refers to an instrument without a spec.
    #[error("no instrument spec for {0}")]
    MissingInstrument(InstrumentId),
    /// A store failed.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// The session failed.
    #[error(transparent)]
    Session(#[from] SessionError),
    /// Invalid settings or data.
    #[error("invalid: {0}")]
    Invalid(String),
}

/// One processed day, summarized.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DaySummary {
    /// Date.
    pub date: NaiveDate,
    /// Equity at the close.
    pub equity: Decimal,
    /// Trades booked.
    pub trades: usize,
    /// Decisions by outcome code.
    pub decisions: std::collections::BTreeMap<String, u32>,
    /// Positions left without protection.
    pub unprotected: u32,
    /// Reconciliation mismatches.
    pub mismatches: u32,
}

impl From<&DayRecord> for DaySummary {
    fn from(d: &DayRecord) -> Self {
        Self {
            date: d.date,
            equity: d.equity,
            trades: d.trades.len(),
            decisions: d.decisions.clone(),
            unprotected: d.unprotected,
            mismatches: d.mismatches,
        }
    }
}

/// The result of a run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PaperRunReport {
    /// The last day processed before this run.
    pub resumed_after: Option<NaiveDate>,
    /// Days processed now.
    pub days: Vec<DaySummary>,
    /// Versions at Paper that were not run, and why.
    pub skipped_versions: Vec<String>,
    /// Unknown intents resolved at restore, and problems doing so.
    pub reconciliation_notes: Vec<String>,
    /// Active positions after the run.
    pub active_positions: usize,
}

pub use qd_strategy::catalog::CatalogEntry;

/// Every strategy implementation in this build.
pub fn catalog() -> Result<Vec<CatalogEntry>, PaperError> {
    qd_strategy::catalog::catalog().map_err(|e| PaperError::Invalid(e.to_string()))
}

/// The paper-trading runner.
pub struct PaperRunner {
    deps: PaperDeps,
    settings: PaperSettings,
    catalog: Vec<CatalogEntry>,
}

impl std::fmt::Debug for PaperRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PaperRunner")
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

/// Journal entries read per page when restoring.
const PAGE: i64 = 1000;

fn product_for(spec: &InstrumentSpec) -> ProductType {
    if spec.kind == InstrumentKind::Future {
        ProductType::Margin
    } else {
        ProductType::Delivery
    }
}

impl PaperRunner {
    /// Creates the runner.
    pub fn new(deps: PaperDeps, settings: PaperSettings) -> Result<Self, PaperError> {
        if settings.initial_equity <= Decimal::ZERO || settings.slippage_ticks < Decimal::ZERO {
            return Err(PaperError::Invalid(
                "initial equity must be positive and slippage non-negative".to_owned(),
            ));
        }
        Ok(Self {
            deps,
            settings,
            catalog: catalog()?,
        })
    }

    /// Rebuilds the account's trading state from the journal (not checked).
    pub async fn load_state(&self) -> Result<RestoredState, PaperError> {
        let mut values = Vec::new();
        let mut after = 0;
        loop {
            let page = self.deps.reader.replay(&STATE_KINDS, after, PAGE).await?;
            let Some(last) = page.last() else { break };
            after = last.seq;
            let full = i64::try_from(page.len()).unwrap_or(0) >= PAGE;
            values.extend(page.into_iter().map(|e| e.entry));
            if !full {
                break;
            }
        }
        RestoredState::from_entries(self.settings.account, values.iter())
            .map_err(PaperError::Restore)
    }

    async fn halt_on_restore_failure(&self, error: &RestoreError) {
        let now = self.deps.clock.now();
        if let Ok(halt) = Halt::new(
            HaltId::new_at(now),
            HaltKind::Operational,
            HaltScope::Account(self.settings.account),
            format!("paper book could not be restored: {error}"),
            now,
            None,
            true,
        ) {
            if let Err(e) = self.deps.halts.record(&halt).await {
                tracing::error!(error = %e, "could not record the restore halt");
            }
        }
    }

    async fn checked_state(&self) -> Result<RestoredState, PaperError> {
        let state = match self.load_state().await {
            Ok(state) => state,
            Err(PaperError::Restore(e)) => {
                self.halt_on_restore_failure(&e).await;
                return Err(PaperError::Restore(e));
            }
            Err(e) => return Err(e),
        };
        if let Err(e) = state.check() {
            self.halt_on_restore_failure(&e).await;
            return Err(PaperError::Restore(e));
        }
        Ok(state)
    }

    async fn slots(&self) -> Result<(Vec<StrategySlot<'_>>, Vec<String>), PaperError> {
        let mut slots = Vec::new();
        let mut skipped = Vec::new();
        let versions = self
            .deps
            .registry
            .versions()
            .await
            .map_err(|e| PaperError::Store(StoreError(e.to_string())))?;
        for v in versions {
            let stage = self
                .deps
                .registry
                .stage(v.reference.version_id)
                .await
                .map_err(|e| PaperError::Store(StoreError(e.to_string())))?;
            if stage != StrategyStage::Paper {
                continue;
            }
            let label = format!("{} v{}", v.reference.name, v.reference.version_number);
            let Some(entry) = self
                .catalog
                .iter()
                .find(|c| c.strategy.logic_version() == v.reference.logic_version)
            else {
                skipped.push(format!("{label}: logic version not in this build"));
                continue;
            };
            if entry.parameters != v.parameters {
                skipped.push(format!(
                    "{label}: registered parameters differ from the logic version's"
                ));
                continue;
            }
            let slippage = SlippageAssumption::new("slip-v1", Decimal::ZERO)
                .map_err(|e| PaperError::Invalid(e.to_string()))?;
            slots.push(StrategySlot {
                strategy: entry.strategy.as_ref(),
                info: StrategyVersionInfo {
                    reference: v.reference.clone(),
                    stage,
                    rr_floor: v.rr_floor,
                    slippage,
                },
                slippage_ticks: Some(self.settings.slippage_ticks),
            });
        }
        Ok((slots, skipped))
    }

    async fn instruments(
        &self,
        through: NaiveDate,
        known_at: chrono::DateTime<Utc>,
    ) -> Result<(Vec<InstrumentSpec>, Vec<InstrumentData>), PaperError> {
        let mut latest: HashMap<InstrumentId, InstrumentSpec> = HashMap::new();
        for spec in self.deps.market.instruments(through).await? {
            let newer = latest
                .get(&spec.id)
                .is_none_or(|s| spec.version > s.version);
            if newer {
                latest.insert(spec.id, spec);
            }
        }
        let mut specs: Vec<InstrumentSpec> = latest.into_values().collect();
        specs.sort_by_key(|s| s.id);
        let from = self.settings.start - Duration::days(self.settings.warm_up_days);
        let mut data = Vec::new();
        for spec in &specs {
            let bars = self
                .deps
                .market
                .daily_bars(spec.id, from, through, known_at)
                .await?;
            let Some(last) = bars.last().map(Bar::date) else {
                continue;
            };
            let series = BarSeries::new(spec.id, last, bars)
                .map_err(|e| PaperError::Invalid(e.to_string()))?;
            data.push(InstrumentData {
                spec: spec.clone(),
                product: product_for(spec),
                series,
            });
        }
        Ok((specs, data))
    }

    /// Processes every trading day after the last processed one, through `through`.
    pub async fn run(&self, through: NaiveDate) -> Result<PaperRunReport, PaperError> {
        let _guard = self
            .deps
            .lock
            .try_acquire(&format!("paper-run:{}", self.settings.account))
            .await?
            .ok_or(PaperError::Busy)?;
        let account = self
            .deps
            .accounts
            .account(self.settings.account)
            .await?
            .ok_or(PaperError::UnknownAccount)?;
        if account.mode != AccountMode::Paper {
            return Err(PaperError::NotPaperAccount);
        }
        let currency =
            Currency::new(&account.currency).map_err(|e| PaperError::Invalid(e.to_string()))?;
        let restored = self.checked_state().await?;
        let now = self.deps.clock.now();
        let (specs, data) = self.instruments(through, now).await?;
        for p in restored.active_positions() {
            if !specs.iter().any(|s| s.id == p.instrument) {
                return Err(PaperError::MissingInstrument(p.instrument));
            }
        }

        let resumed_after = restored.last_day.as_ref().map(|d| d.date);
        let first = resumed_after
            .and_then(|d| d.succ_opt())
            .map_or(self.settings.start, |d| d.max(self.settings.start));
        let dates: BTreeSet<NaiveDate> = data
            .iter()
            .flat_map(|d| d.series.bars().iter().map(Bar::date))
            .filter(|d| *d >= first && *d <= through)
            .collect();
        let close =
            |date: NaiveDate| Utc.from_utc_datetime(&date.and_time(self.settings.close_time_utc));
        let market_clock = Arc::new(SessionClock::new(close(first)));
        let venue = Arc::new(PaperBroker::new(
            market_clock.clone(),
            specs.clone(),
            self.settings.slippage_ticks,
        ));
        let symbols: HashMap<InstrumentId, String> =
            specs.iter().map(|s| (s.id, s.symbol.clone())).collect();
        venue
            .restore(restored.working_orders(&symbols), &restored.net_positions())
            .map_err(|e| PaperError::Invalid(e.to_string()))?;
        let gateway = Arc::new(OrderGateway::new(
            GatewayAccount {
                id: self.settings.account,
                mode: AccountMode::Paper,
                live_armed: false,
            },
            venue.clone(),
            self.deps.journal.clone(),
            self.deps.halts.clone(),
            self.deps.clock.clone(),
            LivePolicy::default(),
            specs,
        ));
        let mut session = TradingSession::restored(
            SessionParts {
                gateway,
                venue,
                journal: self.deps.journal.clone(),
                halts: self.deps.halts.clone(),
                market_clock,
                wall_clock: self.deps.clock.clone(),
                book: AccountBook::new(
                    self.settings.account,
                    Money::new(self.settings.initial_equity, currency),
                    first,
                ),
                settings: SessionSettings {
                    mode: AccountMode::Paper,
                    close_time_utc: self.settings.close_time_utc,
                    calendar_version: self.settings.calendar_version.clone(),
                    snapshot: SnapshotId::new_at(now),
                },
            },
            &restored,
        );
        let reconciliation_notes = session.reconcile_unknown_with_venue().await;
        let (slots, skipped_versions) = self.slots().await?;
        let evidence = self.deps.evidence.load().await?;
        let engine = DecisionEngine::new(
            &self.deps.risk,
            self.deps.costs.as_ref(),
            evidence.as_ref(),
            EvidencePolicy::Required {
                min_evidence: self.deps.risk.min_evidence,
            },
        );
        let mut days = Vec::new();
        for date in dates {
            let day = session
                .process_day(
                    date,
                    &data,
                    &slots,
                    &engine,
                    &self.deps.risk,
                    self.deps.costs.as_ref(),
                )
                .await?;
            days.push(DaySummary::from(&day));
        }
        Ok(PaperRunReport {
            resumed_after,
            days,
            skipped_versions,
            reconciliation_notes,
            active_positions: session.positions().active().len(),
        })
    }

    /// The current book as JSON: account state, positions, working orders.
    pub async fn state_json(&self) -> Result<Value, PaperError> {
        let state = self.load_state().await?;
        let consistent = state.check().map_err(|e| e.to_string()).err();
        let orders: Vec<Value> = state
            .intents
            .iter()
            .filter(|r| {
                matches!(
                    r.state,
                    OrderIntentState::PendingSubmit
                        | OrderIntentState::Submitted
                        | OrderIntentState::PartiallyFilled
                        | OrderIntentState::Unknown
                )
            })
            .map(|r| {
                json!({
                    "intent": r.intent,
                    "state": r.state,
                    "filled": r.filled,
                })
            })
            .collect();
        Ok(json!({
            "account": self.settings.account,
            "last_day": state.last_day,
            "positions": state.positions,
            "working_orders": orders,
            "inconsistency": consistent,
        }))
    }
}

fn store_error(e: &PaperError) -> StoreError {
    StoreError(e.to_string())
}

#[async_trait]
impl PaperTrading for PaperRunner {
    async fn run_through(&self, through: NaiveDate) -> Result<Value, StoreError> {
        let report = self.run(through).await.map_err(|e| store_error(&e))?;
        serde_json::to_value(report).map_err(|e| StoreError(e.to_string()))
    }

    async fn state(&self) -> Result<Value, StoreError> {
        self.state_json().await.map_err(|e| store_error(&e))
    }
}

#[async_trait]
impl Reconciler for PaperRunner {
    async fn reconcile(&self) -> Result<Vec<String>, StoreError> {
        let state = self.load_state().await.map_err(|e| store_error(&e))?;
        if let Err(e) = state.check() {
            return Ok(vec![e.to_string()]);
        }
        Ok(
            reconcile_book(&state.active_positions(), &state.net_positions())
                .into_iter()
                .map(|m| {
                    format!(
                        "instrument {}: book {} vs venue {}",
                        m.instrument, m.book, m.broker
                    )
                })
                .collect(),
        )
    }
}
