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
use qd_app::decision::{DecisionEngine, EvidencePolicy};
use qd_app::gateway::{GatewayAccount, OrderGateway};
use qd_app::live::LivePolicy;
use qd_app::ports::{
    AccountStore, Clock, Evidence, EvidenceSource, HaltStore, HistoricalMarketData, Journal,
    JournalReader, PaperTrading, Reconciler, RunLock, StoreError,
};
use qd_app::positions::reconcile_book;
use qd_app::registry::StrategyRegistry;
use qd_app::restore::{RestoreError, RestoredState};
use qd_app::runs;
use qd_app::session::{
    AccountBook, DayRecord, InstrumentData, SessionClock, SessionError, SessionParts,
    SessionSettings, StrategySlot, TradingSession,
};
use qd_domain::costs::CostModel;
use qd_domain::halt::{Halt, HaltKind, HaltScope};
use qd_domain::ids::{AccountId, HaltId, InstrumentId, SnapshotId, StrategyVersionId};
use qd_domain::instrument::InstrumentSpec;
use qd_domain::lifecycle::strategy::StrategyStage;
use qd_domain::market::Bar;
use qd_domain::num::{Currency, Money};
use qd_domain::proposal::AccountMode;
use qd_risk::config::RiskConfig;
use rust_decimal::Decimal;
use serde::Serialize;
use serde_json::Value;
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

impl From<runs::LoadError> for PaperError {
    fn from(e: runs::LoadError) -> Self {
        match e {
            runs::LoadError::Store(e) => Self::Store(e),
            runs::LoadError::Restore(e) => Self::Restore(e),
            runs::LoadError::Invalid(e) => Self::Invalid(e),
        }
    }
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
        runs::load_state(self.deps.reader.as_ref(), self.settings.account)
            .await
            .map_err(PaperError::from)
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
        Ok(runs::stage_slots(
            &self.deps.registry,
            &self.catalog,
            &[StrategyStage::Paper],
            self.settings.slippage_ticks,
        )
        .await?)
    }

    async fn instruments(
        &self,
        through: NaiveDate,
        known_at: chrono::DateTime<Utc>,
    ) -> Result<(Vec<InstrumentSpec>, Vec<InstrumentData>), PaperError> {
        let from = self.settings.start - Duration::days(self.settings.warm_up_days);
        Ok(runs::load_instruments(self.deps.market.as_ref(), from, through, known_at).await?)
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
        Ok(runs::book_json(self.settings.account, &state))
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
        // A book for an account that does not exist, or is not a paper
        // account, cannot be reconciled: entries stay halted (INV-07).
        match self.deps.accounts.account(self.settings.account).await? {
            None => return Ok(vec!["the configured account does not exist".to_owned()]),
            Some(a) if a.mode != AccountMode::Paper => {
                return Ok(vec![
                    "the configured account is not a paper account".to_owned(),
                ]);
            }
            Some(_) => {}
        }
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
