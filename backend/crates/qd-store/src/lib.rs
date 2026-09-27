//! PostgreSQL adapters for the `qd-app` ports (spec §5.3 `qd-store`).
//!
//! History tables are append-only in the database itself (INV-16; see
//! `backend/migrations`). Queries are runtime-checked `sqlx` queries covered by
//! integration tests against a real PostgreSQL (ADR 0007).

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use qd_app::journal::JournalEntry;
use qd_app::ports::{AuditLog, HaltStore, HistoricalMarketData, Journal, JournalError, StoreError};
use qd_app::registry::{StrategyRegistryStore, StrategyVersionRecord};
use qd_domain::halt::Halt;
use qd_domain::ids::{AccountId, InstrumentId, StrategyVersionId};
use qd_domain::instrument::InstrumentSpec;
use qd_domain::lifecycle::strategy::{StageEvent, StrategyStage};
use qd_domain::market::{Bar, BarData};
use qd_domain::proposal::AccountMode;
use rust_decimal::Decimal;
use serde::Serialize;
use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::{Row, migrate::Migrator};

/// The schema migrations, embedded at build time.
pub static MIGRATOR: Migrator = sqlx::migrate!("../../migrations");

fn store_error(e: impl std::fmt::Display) -> StoreError {
    StoreError(e.to_string())
}

/// Connects to PostgreSQL. The URL comes from the environment, never from Git.
pub async fn connect(url: &str, max_connections: u32) -> Result<PgPool, StoreError> {
    PgPoolOptions::new()
        .max_connections(max_connections)
        .connect(url)
        .await
        .map_err(store_error)
}

/// Applies pending migrations.
pub async fn migrate(pool: &PgPool) -> Result<(), StoreError> {
    MIGRATOR.run(pool).await.map_err(store_error)
}

/// The Decision Journal in PostgreSQL. `append` returns after the commit.
#[derive(Clone, Debug)]
pub struct PgJournal {
    pool: PgPool,
}

impl PgJournal {
    /// Creates the journal.
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// The most recent entries, newest first, as stored JSON.
    pub async fn recent(
        &self,
        limit: i64,
    ) -> Result<Vec<(i64, String, serde_json::Value)>, StoreError> {
        let rows = sqlx::query("SELECT seq, kind, entry FROM journal ORDER BY seq DESC LIMIT $1")
            .bind(limit)
            .fetch_all(&self.pool)
            .await
            .map_err(store_error)?;
        rows.iter()
            .map(|r| {
                Ok((
                    r.try_get("seq").map_err(store_error)?,
                    r.try_get("kind").map_err(store_error)?,
                    r.try_get("entry").map_err(store_error)?,
                ))
            })
            .collect()
    }
}

#[async_trait]
impl Journal for PgJournal {
    async fn append(&self, entry: &JournalEntry) -> Result<u64, JournalError> {
        let json = serde_json::to_value(entry).map_err(|e| JournalError(e.to_string()))?;
        let kind = json
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown")
            .to_owned();
        let seq: i64 =
            sqlx::query_scalar("INSERT INTO journal (kind, entry) VALUES ($1, $2) RETURNING seq")
                .bind(kind)
                .bind(json)
                .fetch_one(&self.pool)
                .await
                .map_err(|e| JournalError(e.to_string()))?;
        u64::try_from(seq).map_err(|e| JournalError(e.to_string()))
    }
}

/// The kill switch in PostgreSQL. Every halt version is a new row; loading
/// takes the latest version of each halt and re-validates it.
#[derive(Clone, Debug)]
pub struct PgHaltStore {
    pool: PgPool,
}

impl PgHaltStore {
    /// Creates the store.
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl HaltStore for PgHaltStore {
    async fn load(&self) -> Result<Vec<Halt>, StoreError> {
        let rows = sqlx::query(
            "SELECT DISTINCT ON (halt_id) halt FROM halt_events ORDER BY halt_id, seq DESC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(store_error)?;
        rows.iter()
            .map(|r| {
                let json: serde_json::Value = r.try_get("halt").map_err(store_error)?;
                // Re-validated on read: a tampered row fails closed.
                serde_json::from_value::<Halt>(json).map_err(store_error)
            })
            .collect()
    }

    async fn record(&self, halt: &Halt) -> Result<(), StoreError> {
        let json = serde_json::to_value(halt).map_err(store_error)?;
        sqlx::query("INSERT INTO halt_events (halt_id, halt) VALUES ($1, $2)")
            .bind(halt.id().as_uuid())
            .bind(json)
            .execute(&self.pool)
            .await
            .map_err(store_error)?;
        Ok(())
    }
}

/// Instrument specs and point-in-time daily bars.
#[derive(Clone, Debug)]
pub struct PgMarketData {
    pool: PgPool,
}

impl PgMarketData {
    /// Creates the store.
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Records an instrument spec version (immutable once stored).
    pub async fn add_instrument(&self, spec: &InstrumentSpec) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO instrument_specs (instrument_id, version, symbol, spec) VALUES ($1, $2, $3, $4)",
        )
        .bind(spec.id.as_uuid())
        .bind(i32::try_from(spec.version).map_err(store_error)?)
        .bind(&spec.symbol)
        .bind(serde_json::to_value(spec).map_err(store_error)?)
        .execute(&self.pool)
        .await
        .map_err(store_error)?;
        Ok(())
    }

    /// Stores bars ingested at `ingested_at`. Returns how many rows were written.
    pub async fn insert_bars(
        &self,
        instrument: InstrumentId,
        bars: &[Bar],
        ingested_at: DateTime<Utc>,
    ) -> Result<u64, StoreError> {
        let mut tx = self.pool.begin().await.map_err(store_error)?;
        let mut written = 0;
        for bar in bars {
            written += sqlx::query(
                "INSERT INTO bars (instrument_id, trade_date, ingested_at, open, high, low, close, volume) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            )
            .bind(instrument.as_uuid())
            .bind(bar.date())
            .bind(ingested_at)
            .bind(bar.open().value())
            .bind(bar.high().value())
            .bind(bar.low().value())
            .bind(bar.close().value())
            .bind(bar.volume())
            .execute(&mut *tx)
            .await
            .map_err(store_error)?
            .rows_affected();
        }
        tx.commit().await.map_err(store_error)?;
        Ok(written)
    }

    /// Every stored spec version of every instrument.
    pub async fn all_instruments(&self) -> Result<Vec<InstrumentSpec>, StoreError> {
        let rows = sqlx::query("SELECT spec FROM instrument_specs ORDER BY symbol, version")
            .fetch_all(&self.pool)
            .await
            .map_err(store_error)?;
        rows.iter()
            .map(|r| {
                let json: serde_json::Value = r.try_get("spec").map_err(store_error)?;
                serde_json::from_value::<InstrumentSpec>(json).map_err(store_error)
            })
            .collect()
    }
}

#[async_trait]
impl HistoricalMarketData for PgMarketData {
    async fn instruments(&self, date: NaiveDate) -> Result<Vec<InstrumentSpec>, StoreError> {
        Ok(self
            .all_instruments()
            .await?
            .into_iter()
            .filter(|s| s.is_effective_on(date))
            .collect())
    }

    async fn daily_bars(
        &self,
        instrument: InstrumentId,
        from: NaiveDate,
        to: NaiveDate,
        known_at: DateTime<Utc>,
    ) -> Result<Vec<Bar>, StoreError> {
        let rows = sqlx::query(
            "SELECT DISTINCT ON (trade_date) trade_date, open, high, low, close, volume \
             FROM bars WHERE instrument_id = $1 AND trade_date BETWEEN $2 AND $3 AND ingested_at <= $4 \
             ORDER BY trade_date, ingested_at DESC",
        )
        .bind(instrument.as_uuid())
        .bind(from)
        .bind(to)
        .bind(known_at)
        .fetch_all(&self.pool)
        .await
        .map_err(store_error)?;
        rows.iter()
            .map(|r| {
                let get = |c: &str| r.try_get::<Decimal, _>(c).map_err(store_error);
                Bar::new(BarData {
                    date: r.try_get("trade_date").map_err(store_error)?,
                    open: get("open")?,
                    high: get("high")?,
                    low: get("low")?,
                    close: get("close")?,
                    volume: get("volume")?,
                })
                .map_err(store_error)
            })
            .collect()
    }
}

/// The Strategy Registry's storage.
#[derive(Clone, Debug)]
pub struct PgStrategyRegistry {
    pool: PgPool,
}

impl PgStrategyRegistry {
    /// Creates the store.
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn version_from_row(r: &sqlx::postgres::PgRow) -> Result<StrategyVersionRecord, StoreError> {
    let reference = qd_domain::proposal::StrategyRef {
        strategy_id: qd_domain::ids::StrategyId::from_uuid(
            r.try_get("strategy_id").map_err(store_error)?,
        ),
        name: r.try_get("name").map_err(store_error)?,
        version_id: StrategyVersionId::from_uuid(r.try_get("version_id").map_err(store_error)?),
        version_number: u32::try_from(r.try_get::<i32, _>("version_number").map_err(store_error)?)
            .map_err(store_error)?,
        logic_version: r.try_get("logic_version").map_err(store_error)?,
        git_sha: r.try_get("git_sha").map_err(store_error)?,
    };
    Ok(StrategyVersionRecord {
        reference,
        parameters: r.try_get("parameters").map_err(store_error)?,
        rr_floor: r.try_get("rr_floor").map_err(store_error)?,
    })
}

const VERSION_COLUMNS: &str =
    "version_id, strategy_id, name, version_number, logic_version, parameters, git_sha, rr_floor";

#[async_trait]
impl StrategyRegistryStore for PgStrategyRegistry {
    async fn register(&self, v: &StrategyVersionRecord) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO strategy_versions \
             (version_id, strategy_id, name, version_number, logic_version, parameters, git_sha, rr_floor) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(v.reference.version_id.as_uuid())
        .bind(v.reference.strategy_id.as_uuid())
        .bind(&v.reference.name)
        .bind(i32::try_from(v.reference.version_number).map_err(store_error)?)
        .bind(&v.reference.logic_version)
        .bind(&v.parameters)
        .bind(&v.reference.git_sha)
        .bind(v.rr_floor)
        .execute(&self.pool)
        .await
        .map_err(store_error)?;
        Ok(())
    }

    async fn version(
        &self,
        id: StrategyVersionId,
    ) -> Result<Option<StrategyVersionRecord>, StoreError> {
        let row = sqlx::query(&format!(
            "SELECT {VERSION_COLUMNS} FROM strategy_versions WHERE version_id = $1"
        ))
        .bind(id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(store_error)?;
        row.as_ref().map(version_from_row).transpose()
    }

    async fn versions(&self) -> Result<Vec<StrategyVersionRecord>, StoreError> {
        let rows = sqlx::query(&format!(
            "SELECT {VERSION_COLUMNS} FROM strategy_versions ORDER BY name, version_number"
        ))
        .fetch_all(&self.pool)
        .await
        .map_err(store_error)?;
        rows.iter().map(version_from_row).collect()
    }

    async fn stage_events(&self, id: StrategyVersionId) -> Result<Vec<StageEvent>, StoreError> {
        let rows = sqlx::query(
            "SELECT event FROM strategy_stage_events WHERE version_id = $1 ORDER BY seq",
        )
        .bind(id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(store_error)?;
        rows.iter()
            .map(|r| {
                let json: serde_json::Value = r.try_get("event").map_err(store_error)?;
                serde_json::from_value(json).map_err(store_error)
            })
            .collect()
    }

    async fn append_stage_event(
        &self,
        id: StrategyVersionId,
        event: &StageEvent,
        stage: StrategyStage,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO strategy_stage_events (version_id, event, stage) VALUES ($1, $2, $3)",
        )
        .bind(id.as_uuid())
        .bind(serde_json::to_value(event).map_err(store_error)?)
        .bind(serde_json::to_value(stage).map_err(store_error)?)
        .execute(&self.pool)
        .await
        .map_err(store_error)?;
        Ok(())
    }
}

/// The audit log in PostgreSQL.
#[derive(Clone, Debug)]
pub struct PgAuditLog {
    pool: PgPool,
}

impl PgAuditLog {
    /// Creates the log.
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl AuditLog for PgAuditLog {
    async fn record(
        &self,
        actor: &str,
        action: &str,
        detail: serde_json::Value,
    ) -> Result<(), StoreError> {
        sqlx::query("INSERT INTO audit_log (actor, action, detail) VALUES ($1, $2, $3)")
            .bind(actor)
            .bind(action)
            .bind(detail)
            .execute(&self.pool)
            .await
            .map_err(store_error)?;
        Ok(())
    }
}

/// One account.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AccountRecord {
    /// Id.
    pub id: AccountId,
    /// Display name.
    pub name: String,
    /// Mode.
    pub mode: AccountMode,
    /// Currency code.
    pub currency: String,
    /// Live armed (INV-14); always false for non-live accounts.
    pub live_armed: bool,
}

fn mode_str(mode: AccountMode) -> &'static str {
    match mode {
        AccountMode::Backtest => "backtest",
        AccountMode::Paper => "paper",
        AccountMode::Live => "live",
    }
}

fn parse_mode(s: &str) -> Result<AccountMode, StoreError> {
    match s {
        "backtest" => Ok(AccountMode::Backtest),
        "paper" => Ok(AccountMode::Paper),
        "live" => Ok(AccountMode::Live),
        other => Err(StoreError(format!("unknown account mode {other}"))),
    }
}

/// Accounts. Every change is written to the audit log in the same transaction.
#[derive(Clone, Debug)]
pub struct PgAccounts {
    pool: PgPool,
}

impl PgAccounts {
    /// Creates the store.
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Creates an account (never armed).
    pub async fn create(&self, account: &AccountRecord, actor: &str) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await.map_err(store_error)?;
        sqlx::query("INSERT INTO accounts (account_id, name, mode, currency, live_armed) VALUES ($1, $2, $3, $4, false)")
            .bind(account.id.as_uuid())
            .bind(&account.name)
            .bind(mode_str(account.mode))
            .bind(&account.currency)
            .execute(&mut *tx)
            .await
            .map_err(store_error)?;
        sqlx::query(
            "INSERT INTO audit_log (actor, action, detail) VALUES ($1, 'account.create', $2)",
        )
        .bind(actor)
        .bind(serde_json::to_value(account).map_err(store_error)?)
        .execute(&mut *tx)
        .await
        .map_err(store_error)?;
        tx.commit().await.map_err(store_error)
    }

    /// Loads an account.
    pub async fn get(&self, id: AccountId) -> Result<Option<AccountRecord>, StoreError> {
        let row = sqlx::query("SELECT account_id, name, mode, currency, live_armed FROM accounts WHERE account_id = $1")
            .bind(id.as_uuid())
            .fetch_optional(&self.pool)
            .await
            .map_err(store_error)?;
        row.map(|r| {
            Ok(AccountRecord {
                id: AccountId::from_uuid(r.try_get("account_id").map_err(store_error)?),
                name: r.try_get("name").map_err(store_error)?,
                mode: parse_mode(&r.try_get::<String, _>("mode").map_err(store_error)?)?,
                currency: r.try_get("currency").map_err(store_error)?,
                live_armed: r.try_get("live_armed").map_err(store_error)?,
            })
        })
        .transpose()
    }

    /// Arms or disarms live trading. The caller must have verified a step-up
    /// authenticated owner action (Phase 5). Non-live accounts cannot be armed
    /// (a database constraint).
    pub async fn set_live_armed(
        &self,
        id: AccountId,
        armed: bool,
        actor: &str,
    ) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await.map_err(store_error)?;
        let updated = sqlx::query(
            "UPDATE accounts SET live_armed = $2, updated_at = now() WHERE account_id = $1",
        )
        .bind(id.as_uuid())
        .bind(armed)
        .execute(&mut *tx)
        .await
        .map_err(store_error)?
        .rows_affected();
        if updated != 1 {
            return Err(StoreError("unknown account".to_owned()));
        }
        sqlx::query(
            "INSERT INTO audit_log (actor, action, detail) VALUES ($1, 'account.live_armed', $2)",
        )
        .bind(actor)
        .bind(serde_json::json!({ "account": id, "armed": armed }))
        .execute(&mut *tx)
        .await
        .map_err(store_error)?;
        tx.commit().await.map_err(store_error)
    }
}

/// All PostgreSQL adapters over one pool.
#[derive(Clone, Debug)]
pub struct Stores {
    /// Journal.
    pub journal: Arc<PgJournal>,
    /// Halts.
    pub halts: Arc<PgHaltStore>,
    /// Market data.
    pub market: Arc<PgMarketData>,
    /// Strategy registry storage.
    pub registry: Arc<PgStrategyRegistry>,
    /// Audit log.
    pub audit: Arc<PgAuditLog>,
    /// Accounts.
    pub accounts: Arc<PgAccounts>,
}

impl Stores {
    /// Creates every adapter over `pool`.
    #[must_use]
    pub fn new(pool: &PgPool) -> Self {
        Self {
            journal: Arc::new(PgJournal::new(pool.clone())),
            halts: Arc::new(PgHaltStore::new(pool.clone())),
            market: Arc::new(PgMarketData::new(pool.clone())),
            registry: Arc::new(PgStrategyRegistry::new(pool.clone())),
            audit: Arc::new(PgAuditLog::new(pool.clone())),
            accounts: Arc::new(PgAccounts::new(pool.clone())),
        }
    }
}
