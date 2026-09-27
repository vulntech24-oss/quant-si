//! The live-trading runner at Zerodha (ADR 0014).
//!
//! Two operations, both under the account's run lock:
//!
//! - [`LiveRunner::sync`], during market hours: read fills from Kite and
//!   apply them through the Order Gateway and Position Manager, so a filled
//!   entry gets its protective GTT at once. No decisions.
//! - [`LiveRunner::run`], after the close: the daily cycle shared with
//!   backtests and paper (INV-08) for the latest completed date only. Missed
//!   older dates are never traded on: their data is stale for new orders.
//!
//! Every run first checks the book (an inconsistent book records an
//! operational halt), and demotes live strategy versions while a hard halt
//! covers them (INV-11: demotion on breach is automatic). A book that
//! disagrees with Kite halts entries (the session does this in live mode).
//! Orders pass the INV-14 gate in the Order Gateway; without every
//! condition, entries are refused there and exits still go out.

use std::collections::BTreeSet;
use std::sync::Arc;

use chrono::{Duration, NaiveDate, NaiveTime, TimeZone, Utc};
use qd_app::decision::{DecisionEngine, EvidencePolicy};
use qd_app::evidence::EvidenceLoader;
use qd_app::gateway::{GatewayAccount, OrderGateway};
use qd_app::live::LivePolicy;
use qd_app::ports::AgentExecution;
use qd_app::ports::{
    AccountRecord, AccountStore, Clock, HaltStore, HistoricalMarketData, Journal, JournalReader,
    RunLock, StoreError,
};
use qd_app::positions::PositionUpdate;
use qd_app::registry::StrategyRegistry;
use qd_app::restore::{RestoreError, RestoredState};
use qd_app::runs::{self, LoadError};
use qd_app::session::{
    AccountBook, AgentEntry, InstrumentData, SessionClock, SessionError, SessionParts,
    SessionSettings, TradingSession,
};
use qd_domain::costs::CostModel;
use qd_domain::halt::{Halt, HaltKind, HaltScope};
use qd_domain::ids::{AccountId, HaltId, PositionId, SnapshotId, StrategyVersionId};
use qd_domain::instrument::InstrumentSpec;
use qd_domain::lifecycle::strategy::{StageEvent, StrategyStage};
use qd_domain::market::Bar;
use qd_domain::num::{Currency, Money};
use qd_domain::proposal::AccountMode;
use qd_risk::config::RiskConfig;
use qd_strategy::catalog::CatalogEntry;
use rust_decimal::Decimal;
use serde::Serialize;
use thiserror::Error;

use crate::broker::{KiteBroker, OrderSettings, RefreshReport};
use crate::client::{KiteClient, KiteError};
use crate::market::ist;

/// A source with no evidence tables (agent entries bring their own forecast).
struct NoEvidence;

impl qd_app::ports::EvidenceSource for NoEvidence {
    fn evidence(
        &self,
        _: qd_domain::ids::StrategyVersionId,
        _: &str,
    ) -> Option<qd_app::ports::Evidence> {
        None
    }
}

/// The stages that trade live.
pub const LIVE_STAGES: [StrategyStage; 2] = [StrategyStage::SmallCapital, StrategyStage::Full];

/// A missed date older than this many calendar days is not traded on.
const MAX_STALE_DAYS: i64 = 4;

/// Live settings (server configuration `[live]`, not editable in the UI).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveSettings {
    /// The live account.
    pub account: AccountId,
    /// Capital the book starts from, account currency.
    pub initial_equity: Decimal,
    /// First date the live book may process.
    pub start: NaiveDate,
    /// Time of day (UTC) at which a date's bar counts as complete.
    pub close_time_utc: NaiveTime,
    /// Stop-slippage assumption for decisions, in ticks.
    pub slippage_ticks: Decimal,
    /// Calendar days of history loaded for indicators.
    pub warm_up_days: i64,
    /// Trading-calendar version recorded with decisions.
    pub calendar_version: String,
}

/// What the runner uses.
#[derive(Clone)]
pub struct LiveDeps {
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
    /// Evidence tables.
    pub evidence: Arc<dyn EvidenceLoader>,
    /// Run lock.
    pub lock: Arc<dyn RunLock>,
    /// Wall clock.
    pub clock: Arc<dyn Clock>,
    /// The INV-14 policy from the server configuration.
    pub live: LivePolicy,
}

/// Why a live run failed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum LiveError {
    /// Another run holds the lock.
    #[error("another live run is in progress")]
    Busy,
    /// The configured account does not exist.
    #[error("the live account does not exist")]
    UnknownAccount,
    /// The configured account is not a live account.
    #[error("the configured live account is not a live account")]
    NotLiveAccount,
    /// The journal state could not be restored; an operational halt was recorded.
    #[error("restore failed: {0}")]
    Restore(RestoreError),
    /// Kite failed.
    #[error(transparent)]
    Kite(#[from] KiteError),
    /// The newest bars are too old to trade on.
    #[error("the newest bars ({0}) are too old to trade on; sync bars first")]
    Stale(NaiveDate),
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

impl From<LoadError> for LiveError {
    fn from(e: LoadError) -> Self {
        match e {
            LoadError::Store(e) => Self::Store(e),
            LoadError::Restore(e) => Self::Restore(e),
            LoadError::Invalid(e) => Self::Invalid(e),
        }
    }
}

/// The result of a run or a sync.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct LiveReport {
    /// The date processed by a daily run.
    pub processed: Option<NaiveDate>,
    /// What the Kite refresh found.
    pub refresh: RefreshReport,
    /// Versions demoted because a hard halt covers them.
    pub demoted: Vec<String>,
    /// Live versions that were not run, and why.
    pub skipped_versions: Vec<String>,
    /// Problems resolving unknown orders.
    pub reconciliation_notes: Vec<String>,
    /// Decisions by outcome code.
    pub decisions: std::collections::BTreeMap<String, u32>,
    /// Positions without protection.
    pub unprotected: u32,
    /// Position mismatches with Kite.
    pub mismatches: u32,
}

/// Demotes every version at SmallCapital or Full that an active hard halt
/// covers (global, the account, or the version itself) back to Paper.
pub async fn demote_on_breach(
    registry: &StrategyRegistry,
    halts: &dyn HaltStore,
    account: AccountId,
    now: chrono::DateTime<Utc>,
) -> Result<Vec<String>, LiveError> {
    let hard: Vec<Halt> = halts
        .load()
        .await?
        .into_iter()
        .filter(|h| h.kind() == HaltKind::HardHalt && h.is_active_at(now))
        .collect();
    if hard.is_empty() {
        return Ok(Vec::new());
    }
    let store = |e: qd_app::registry::RegistryError| LiveError::Store(StoreError(e.to_string()));
    let mut demoted = Vec::new();
    for v in registry.versions().await.map_err(store)? {
        let id = v.reference.version_id;
        if !LIVE_STAGES.contains(&registry.stage(id).await.map_err(store)?) {
            continue;
        }
        let breach = hard.iter().find(|h| match h.scope() {
            HaltScope::Global => true,
            HaltScope::Account(a) => a == account,
            HaltScope::StrategyVersion(s) => s == id,
            HaltScope::Instrument(_) => false,
        });
        if let Some(halt) = breach {
            registry
                .transition(
                    id,
                    &StageEvent::AutoDemote {
                        reason: format!("hard halt: {}", halt.reason()),
                    },
                    "system",
                )
                .await
                .map_err(store)?;
            demoted.push(format!(
                "{} v{}",
                v.reference.name, v.reference.version_number
            ));
        }
    }
    Ok(demoted)
}

/// The live runner.
pub struct LiveRunner {
    deps: LiveDeps,
    settings: LiveSettings,
    catalog: Vec<CatalogEntry>,
}

impl std::fmt::Debug for LiveRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveRunner")
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

struct Prepared {
    session: TradingSession,
    broker: Arc<KiteBroker>,
    data: Vec<InstrumentData>,
    restored: RestoredState,
    notes: Vec<String>,
}

impl LiveRunner {
    /// Creates the runner.
    pub fn new(deps: LiveDeps, settings: LiveSettings) -> Result<Self, LiveError> {
        if settings.initial_equity <= Decimal::ZERO || settings.slippage_ticks < Decimal::ZERO {
            return Err(LiveError::Invalid(
                "initial equity must be positive and slippage non-negative".to_owned(),
            ));
        }
        let catalog =
            qd_strategy::catalog::catalog().map_err(|e| LiveError::Invalid(e.to_string()))?;
        Ok(Self {
            deps,
            settings,
            catalog,
        })
    }

    async fn halt(&self, reason: String) {
        let now = self.deps.clock.now();
        if let Ok(halt) = Halt::new(
            HaltId::new_at(now),
            HaltKind::Operational,
            HaltScope::Account(self.settings.account),
            reason,
            now,
            None,
            true,
        ) {
            if let Err(e) = self.deps.halts.record(&halt).await {
                tracing::error!(error = %e, "could not record a live halt");
            }
        }
    }

    async fn account(&self) -> Result<AccountRecord, LiveError> {
        let account = self
            .deps
            .accounts
            .account(self.settings.account)
            .await?
            .ok_or(LiveError::UnknownAccount)?;
        if account.mode != AccountMode::Live {
            return Err(LiveError::NotLiveAccount);
        }
        Ok(account)
    }

    async fn checked_state(&self) -> Result<RestoredState, LiveError> {
        let state = runs::load_state(self.deps.reader.as_ref(), self.settings.account).await;
        let state = match state {
            Ok(s) => s,
            Err(LoadError::Restore(e)) => {
                self.halt(format!("live book could not be restored: {e}"))
                    .await;
                return Err(LiveError::Restore(e));
            }
            Err(e) => return Err(e.into()),
        };
        if let Err(e) = state.check() {
            self.halt(format!("live book could not be restored: {e}"))
                .await;
            return Err(LiveError::Restore(e));
        }
        Ok(state)
    }

    /// The live book, restored, for the state endpoint (not checked).
    pub async fn state(&self) -> Result<RestoredState, LiveError> {
        Ok(runs::load_state(self.deps.reader.as_ref(), self.settings.account).await?)
    }

    async fn prepare(
        &self,
        client: KiteClient,
        orders: OrderSettings,
        through: NaiveDate,
    ) -> Result<Prepared, LiveError> {
        orders.validate().map_err(LiveError::Invalid)?;
        let account = self.account().await?;
        let currency =
            Currency::new(&account.currency).map_err(|e| LiveError::Invalid(e.to_string()))?;
        let restored = self.checked_state().await?;
        let now = self.deps.clock.now();
        let from = self.settings.start - Duration::days(self.settings.warm_up_days);
        let (specs, data): (Vec<InstrumentSpec>, Vec<InstrumentData>) =
            runs::load_instruments(self.deps.market.as_ref(), from, through, now).await?;
        for p in restored.active_positions() {
            if !specs.iter().any(|s| s.id == p.instrument) {
                return Err(LiveError::Invalid(format!(
                    "no instrument spec for {}",
                    p.instrument
                )));
            }
        }
        let broker = Arc::new(KiteBroker::new(
            client,
            orders,
            self.deps.clock.clone(),
            &specs,
        ));
        broker.restore(&restored);
        let close =
            |date: NaiveDate| Utc.from_utc_datetime(&date.and_time(self.settings.close_time_utc));
        let market_clock = Arc::new(SessionClock::new(close(through)));
        let gateway = Arc::new(OrderGateway::new(
            GatewayAccount {
                id: self.settings.account,
                mode: AccountMode::Live,
                live_armed: account.live_armed,
            },
            broker.clone(),
            self.deps.journal.clone(),
            self.deps.halts.clone(),
            self.deps.clock.clone(),
            self.deps.live,
            specs,
        ));
        let session = TradingSession::restored(
            SessionParts {
                gateway: gateway.clone(),
                venue: broker.clone(),
                journal: self.deps.journal.clone(),
                halts: self.deps.halts.clone(),
                market_clock,
                wall_clock: self.deps.clock.clone(),
                book: AccountBook::new(
                    self.settings.account,
                    Money::new(self.settings.initial_equity, currency),
                    self.settings.start,
                ),
                settings: SessionSettings {
                    mode: AccountMode::Live,
                    close_time_utc: self.settings.close_time_utc,
                    calendar_version: self.settings.calendar_version.clone(),
                    snapshot: SnapshotId::new_at(now),
                },
            },
            &restored,
        );
        // Orders whose outcome was not journaled: look them up at Kite.
        let mut notes = Vec::new();
        let unknown = gateway.unknown_intents();
        if !unknown.is_empty() {
            let found = broker.find_unknown(&unknown).await?;
            for (id, observed) in &found {
                if let Err(e) = gateway.reconcile_intent(*id, *observed).await {
                    notes.push(format!("intent {id}: {e}"));
                }
                if *observed != qd_domain::lifecycle::order::ReconciledState::Submitted {
                    session.positions().on_entry_ended(*id).await;
                }
            }
            let missing = unknown.len() - found.len();
            if missing > 0 {
                let note = format!(
                    "{missing} order(s) with an unknown outcome are not in today's Kite order book; check Kite, then re-arm"
                );
                self.halt(note.clone()).await;
                notes.push(note);
            }
        }
        Ok(Prepared {
            session,
            broker,
            data,
            restored,
            notes,
        })
    }

    /// Applies Kite fills now (market hours), placing protection for filled entries.
    pub async fn sync(
        &self,
        client: KiteClient,
        orders: OrderSettings,
    ) -> Result<LiveReport, LiveError> {
        let _guard = self
            .deps
            .lock
            .try_acquire(&format!("live-run:{}", self.settings.account))
            .await?
            .ok_or(LiveError::Busy)?;
        let now = self.deps.clock.now();
        let today = now.with_timezone(&ist()).date_naive();
        let demoted = demote_on_breach(
            &self.deps.registry,
            self.deps.halts.as_ref(),
            self.settings.account,
            now,
        )
        .await?;
        let prepared = self.prepare(client, orders, today).await?;
        let refresh = prepared.broker.refresh().await?;
        let mut report = LiveReport {
            refresh,
            demoted,
            reconciliation_notes: prepared.notes,
            ..LiveReport::default()
        };
        for (_, events) in prepared.broker.drain_all() {
            let day = prepared.session.apply_events_now(events, today).await;
            report.unprotected += day.unprotected;
        }
        Ok(report)
    }

    /// The daily cycle for the latest completed, unprocessed date through `through`.
    pub async fn run(
        &self,
        client: KiteClient,
        orders: OrderSettings,
        through: NaiveDate,
    ) -> Result<LiveReport, LiveError> {
        let _guard = self
            .deps
            .lock
            .try_acquire(&format!("live-run:{}", self.settings.account))
            .await?
            .ok_or(LiveError::Busy)?;
        let now = self.deps.clock.now();
        let mut demoted = demote_on_breach(
            &self.deps.registry,
            self.deps.halts.as_ref(),
            self.settings.account,
            now,
        )
        .await?;
        let mut prepared = self.prepare(client, orders, through).await?;
        let refresh = prepared.broker.refresh().await?;
        let last = prepared.restored.last_day.as_ref().map(|d| d.date);
        let dates: BTreeSet<NaiveDate> = prepared
            .data
            .iter()
            .flat_map(|d| d.series.bars().iter().map(Bar::date))
            .filter(|d| *d >= self.settings.start && *d <= through && last.is_none_or(|l| *d > l))
            .collect();
        let mut report = LiveReport {
            refresh,
            reconciliation_notes: std::mem::take(&mut prepared.notes),
            ..LiveReport::default()
        };
        let Some(date) = dates.last().copied() else {
            // Nothing new: still apply fills so protection goes out.
            for (_, events) in prepared.broker.drain_all() {
                let day = prepared.session.apply_events_now(events, through).await;
                report.unprotected += day.unprotected;
            }
            report.demoted = demoted;
            return Ok(report);
        };
        if (through - date).num_days() > MAX_STALE_DAYS {
            return Err(LiveError::Stale(date));
        }
        let (slots, skipped) = runs::stage_slots(
            &self.deps.registry,
            &self.catalog,
            &LIVE_STAGES,
            self.settings.slippage_ticks,
        )
        .await?;
        let evidence = self.deps.evidence.load().await?;
        let engine = DecisionEngine::new(
            &self.deps.risk,
            self.deps.costs.as_ref(),
            evidence.as_ref(),
            EvidencePolicy::Required {
                min_evidence: self.deps.risk.min_evidence,
            },
        );
        let day = prepared
            .session
            .process_day(
                date,
                &prepared.data,
                &slots,
                &engine,
                &self.deps.risk,
                self.deps.costs.as_ref(),
            )
            .await?;
        // Fills for instruments without a bar on that date.
        for (_, events) in prepared.broker.drain_all() {
            let extra = prepared.session.apply_events_now(events, date).await;
            report.unprotected += extra.unprotected;
        }
        demoted.extend(
            demote_on_breach(
                &self.deps.registry,
                self.deps.halts.as_ref(),
                self.settings.account,
                self.deps.clock.now(),
            )
            .await?,
        );
        report.processed = Some(date);
        report.demoted = demoted;
        report.skipped_versions = skipped;
        report.decisions = day.decisions;
        report.unprotected += day.unprotected;
        report.mismatches = day.mismatches;
        Ok(report)
    }

    /// An AI agent entry on the live book (ADR 0016), decided after the
    /// close of the last processed day. The Risk Gate needs a live stage for
    /// a live account and the Order Gateway checks every INV-14 condition;
    /// without them the entry is NO TRADE or refused, never sent.
    pub async fn agent_enter(
        &self,
        client: KiteClient,
        orders: OrderSettings,
        entry: &AgentEntry,
    ) -> Result<AgentExecution, LiveError> {
        let _guard = self
            .deps
            .lock
            .try_acquire(&format!("live-run:{}", self.settings.account))
            .await?
            .ok_or(LiveError::Busy)?;
        let today = self.deps.clock.now().with_timezone(&ist()).date_naive();
        let mut prepared = self.prepare(client, orders, today).await?;
        prepared.broker.refresh().await?;
        for (_, events) in prepared.broker.drain_all() {
            prepared.session.apply_events_now(events, today).await;
        }
        let date = prepared
            .restored
            .last_day
            .as_ref()
            .map(|d| d.date)
            .ok_or_else(|| {
                LiveError::Invalid("the live book has no processed day yet".to_owned())
            })?;
        if (today - date).num_days() > MAX_STALE_DAYS {
            return Err(LiveError::Stale(date));
        }
        let evidence = NoEvidence;
        let engine = DecisionEngine::new(
            &self.deps.risk,
            self.deps.costs.as_ref(),
            &evidence,
            EvidencePolicy::Required {
                min_evidence: self.deps.risk.min_evidence,
            },
        );
        let outcome = prepared
            .session
            .agent_entry(date, &prepared.data, entry, &engine, &self.deps.risk)
            .await
            .map_err(|e| LiveError::Invalid(e.to_string()))?;
        Ok(AgentExecution::from_outcome("live", &outcome))
    }

    /// Closes an agent position on the live book (exits are never blocked
    /// by halts or disarming, INV-02).
    pub async fn agent_exit(
        &self,
        client: KiteClient,
        orders: OrderSettings,
        position: PositionId,
        agent: StrategyVersionId,
    ) -> Result<PositionUpdate, LiveError> {
        let _guard = self
            .deps
            .lock
            .try_acquire(&format!("live-run:{}", self.settings.account))
            .await?
            .ok_or(LiveError::Busy)?;
        let today = self.deps.clock.now().with_timezone(&ist()).date_naive();
        let prepared = self.prepare(client, orders, today).await?;
        prepared.broker.refresh().await?;
        for (_, events) in prepared.broker.drain_all() {
            prepared.session.apply_events_now(events, today).await;
        }
        prepared
            .session
            .agent_exit(position, agent)
            .await
            .map_err(|e| LiveError::Invalid(e.to_string()))
    }
}
