//! Runtime settings (ADR 0013): the configuration files give the defaults,
//! settings saved from the web UI override them, and every run reads the
//! effective values. No restart is needed after a change.
//!
//! Editable sections: `paper`, `risk`, `ai`, `kite`, `notifications`,
//! `validation`, `review`. Each
//! save is validated through the same typed constructors the files use,
//! needs the owner's step-up (checked by the API), is stored as a new
//! append-only version, and is audited with old and new values.
//!
//! Not editable here, on purpose: `environment`, `live_trading_enabled`,
//! the bind address, the account and the database URL. Turning real money
//! on stays a deliberate act on the server (INV-14).

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::NaiveDate;
use qd_ai::checklist::ChecklistAdvisor;
use qd_ai::orchestrator::{AiOrchestrator, AiSettings};
use qd_ai::service::AiService;
use qd_app::evidence::StoreEvidenceLoader;
use qd_app::ports::{
    AiAdvisory, BacktestRequest, BacktestRunner, Clock, PaperTrading, Reconciler, SettingVersion,
    SettingsAdmin, SettingsError, SettingsStore, StoreError, ValidationRequest, Validator,
};
use qd_app::registry::StrategyRegistry;
use qd_app::review::{JournalReviewer, ReviewCriteria, Reviewer};
use qd_backtest::validation::ValidationCriteria;
use qd_broker_paper::{PaperDeps, PaperRunner, PaperSettings};
use qd_risk::config::{RiskConfig, RiskConfigData};
use qd_store::Stores;
use serde_json::{Map, Value, json};

use crate::config::{
    AiConfig, DataConfig, KiteConfig, NotificationsConfig, PaperConfig, ServerConfig,
};
use qd_app::ports::SecretReader;
use qd_store::settings::PgSecrets;

/// The editable sections, in display order.
pub const SECTIONS: [(&str, &str, &str); 8] = [
    (
        "paper",
        "Paper trading",
        "Simulated trading on completed daily bars. Changes apply from the next run.",
    ),
    (
        "risk",
        "Risk limits",
        "Every entry passes these limits (INV-03). Loosening them is your decision; it is audited.",
    ),
    (
        "ai",
        "Advisory AI",
        "Shadow mode only: advice never changes orders, sizes, limits or halts (INV-04).",
    ),
    (
        "kite",
        "Zerodha Kite",
        "Market data, daily login and order settings. Live orders also need the server's live settings (INV-14).",
    ),
    (
        "data",
        "Data quality",
        "Checks on every bar import: gaps against the exchange calendar and suspect price jumps.",
    ),
    (
        "notifications",
        "Notifications",
        "Telegram messages for alerts and daily summaries. The bot token is under API keys.",
    ),
    (
        "validation",
        "Validation criteria",
        "What a strategy version must show out of sample before it can reach Paper.",
    ),
    (
        "review",
        "Paper-review criteria",
        "What paper results must show before a version can reach a live stage.",
    ),
];

/// Effective settings: file defaults with saved overrides applied.
#[derive(Clone, Debug)]
pub struct Effective {
    /// Risk configuration.
    pub risk: RiskConfig,
    /// Paper trading, if on.
    pub paper: Option<PaperConfig>,
    /// Advisory AI.
    pub ai: AiConfig,
    /// Validation criteria.
    pub validation: ValidationCriteria,
    /// Paper-review criteria.
    pub review: ReviewCriteria,
    /// Zerodha Kite connection.
    pub kite: KiteConfig,
    /// Notifications.
    pub notifications: NotificationsConfig,
    /// Data-quality limits.
    pub data: DataConfig,
}

/// The server's runtime: file configuration, stores, secrets and clock.
pub struct Runtime {
    /// Configuration from the files.
    pub config: ServerConfig,
    /// Stores.
    pub stores: Stores,
    /// Encrypted secrets (read by server-side adapters only).
    pub secrets: Arc<PgSecrets>,
    /// The Kite API base ([`qd_broker_kite::client::API_BASE`]; tests use a fake).
    pub kite_base: String,
    /// Clock.
    pub clock: Arc<dyn Clock>,
}

impl Runtime {
    /// A runtime over the stores, with the secrets store keyed by the
    /// configuration's master key.
    #[must_use]
    pub fn new(config: ServerConfig, stores: Stores, clock: Arc<dyn Clock>) -> Self {
        let secrets = Arc::new(PgSecrets::new(
            stores.pool.clone(),
            config.master_key.clone(),
            stores.audit.clone(),
        ));
        Self {
            config,
            stores,
            secrets,
            kite_base: qd_broker_kite::client::API_BASE.to_owned(),
            clock,
        }
    }
}

impl std::fmt::Debug for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runtime").finish_non_exhaustive()
    }
}

fn error(e: impl std::fmt::Display) -> StoreError {
    StoreError(e.to_string())
}

/// The paper section's default when the file has none: off, with a template.
fn paper_template(today: NaiveDate) -> Value {
    json!({
        "enabled": false,
        "initial_equity": "1000000",
        "start_date": today,
        "close_time_utc": "10:00:00",
        "slippage_ticks": "1",
        "warm_up_days": 400,
        "daily_run_utc": null,
    })
}

/// Parses and validates one section's JSON, returning it normalized.
fn validate_section(section: &str, value: &Value) -> Result<Value, String> {
    let bad = |e: &dyn std::fmt::Display| format!("{section}: {e}");
    match section {
        "paper" => {
            let mut map = value
                .as_object()
                .cloned()
                .ok_or("paper: expected an object")?;
            let enabled = map
                .remove("enabled")
                .and_then(|v| v.as_bool())
                .ok_or("paper: `enabled` must be true or false")?;
            let paper: PaperConfig =
                serde_json::from_value(Value::Object(map)).map_err(|e| bad(&e))?;
            paper.validate()?;
            let mut out = serde_json::to_value(&paper).map_err(|e| bad(&e))?;
            if let Some(o) = out.as_object_mut() {
                o.insert("enabled".to_owned(), Value::Bool(enabled));
            }
            Ok(out)
        }
        "risk" => {
            let data: RiskConfigData =
                serde_json::from_value(value.clone()).map_err(|e| bad(&e))?;
            let config = RiskConfig::new(data).map_err(|e| bad(&e))?;
            serde_json::to_value(RiskConfigData::clone(&config)).map_err(|e| bad(&e))
        }
        "ai" => {
            let ai: AiConfig = serde_json::from_value(value.clone()).map_err(|e| bad(&e))?;
            ai.validate()?;
            serde_json::to_value(&ai).map_err(|e| bad(&e))
        }
        "validation" => {
            let c: ValidationCriteria =
                serde_json::from_value(value.clone()).map_err(|e| bad(&e))?;
            c.validate()?;
            serde_json::to_value(&c).map_err(|e| bad(&e))
        }
        "review" => {
            let c: ReviewCriteria = serde_json::from_value(value.clone()).map_err(|e| bad(&e))?;
            c.validate()?;
            serde_json::to_value(&c).map_err(|e| bad(&e))
        }
        "kite" => {
            let c: KiteConfig = serde_json::from_value(value.clone()).map_err(|e| bad(&e))?;
            c.validate()?;
            serde_json::to_value(&c).map_err(|e| bad(&e))
        }
        "data" => {
            let c: DataConfig = serde_json::from_value(value.clone()).map_err(|e| bad(&e))?;
            c.validate()?;
            serde_json::to_value(&c).map_err(|e| bad(&e))
        }
        "notifications" => {
            let c: NotificationsConfig =
                serde_json::from_value(value.clone()).map_err(|e| bad(&e))?;
            c.validate()?;
            serde_json::to_value(&c).map_err(|e| bad(&e))
        }
        other => Err(format!("unknown settings section {other}")),
    }
}

impl Runtime {
    /// The file default of a section, as JSON.
    fn file_value(&self, section: &str) -> Result<Value, StoreError> {
        let c = &self.config;
        match section {
            "paper" => Ok(match &c.file.paper {
                Some(p) => {
                    let mut v = serde_json::to_value(p).map_err(error)?;
                    if let Some(o) = v.as_object_mut() {
                        o.insert("enabled".to_owned(), Value::Bool(true));
                    }
                    v
                }
                None => paper_template(self.clock.now().date_naive()),
            }),
            "risk" => serde_json::to_value(RiskConfigData::clone(&c.risk)).map_err(error),
            "ai" => serde_json::to_value(&c.file.ai).map_err(error),
            "validation" => serde_json::to_value(&c.validation).map_err(error),
            "review" => serde_json::to_value(&c.review).map_err(error),
            "kite" => serde_json::to_value(&c.file.kite).map_err(error),
            "notifications" => serde_json::to_value(&c.file.notifications).map_err(error),
            "data" => serde_json::to_value(&c.file.data).map_err(error),
            other => Err(StoreError(format!("unknown settings section {other}"))),
        }
    }

    async fn saved(&self) -> Result<HashMap<String, SettingVersion>, StoreError> {
        Ok(self
            .stores
            .settings
            .latest()
            .await?
            .into_iter()
            .map(|v| (v.section.clone(), v))
            .collect())
    }

    /// A section's current JSON: the saved value, else the file default.
    fn current(
        &self,
        section: &str,
        saved: &HashMap<String, SettingVersion>,
    ) -> Result<Value, StoreError> {
        match saved.get(section).and_then(|v| v.value.clone()) {
            Some(v) => Ok(v),
            None => self.file_value(section),
        }
    }

    /// The effective settings. A saved value that no longer validates is an
    /// error, so every run that needs settings is refused (fail closed).
    pub async fn effective(&self) -> Result<Effective, StoreError> {
        let saved = self.saved().await?;
        let get = |section: &str| -> Result<Value, StoreError> {
            let v = self.current(section, &saved)?;
            validate_section(section, &v).map_err(StoreError)
        };
        let paper = get("paper")?;
        let paper = if paper.get("enabled").and_then(Value::as_bool) == Some(true) {
            let mut map = paper.as_object().cloned().unwrap_or_default();
            map.remove("enabled");
            Some(serde_json::from_value(Value::Object(map)).map_err(error)?)
        } else {
            None
        };
        Ok(Effective {
            risk: RiskConfig::new(serde_json::from_value(get("risk")?).map_err(error)?)
                .map_err(error)?,
            paper,
            ai: serde_json::from_value(get("ai")?).map_err(error)?,
            validation: serde_json::from_value(get("validation")?).map_err(error)?,
            review: serde_json::from_value(get("review")?).map_err(error)?,
            kite: serde_json::from_value(get("kite")?).map_err(error)?,
            notifications: serde_json::from_value(get("notifications")?).map_err(error)?,
            data: serde_json::from_value(get("data")?).map_err(error)?,
        })
    }

    fn registry(&self) -> StrategyRegistry {
        StrategyRegistry::new(
            self.stores.registry.clone(),
            self.stores.audit.clone(),
            self.stores.evidence.clone(),
        )
    }

    /// The paper runner for the effective settings, if paper trading is on.
    pub async fn paper_runner(&self) -> Result<Option<PaperRunner>, StoreError> {
        let e = self.effective().await?;
        let Some(paper) = e.paper else {
            return Ok(None);
        };
        let s = &self.stores;
        let deps = PaperDeps {
            journal: s.journal.clone(),
            reader: s.journal.clone(),
            halts: s.halts.clone(),
            market: s.market.clone(),
            registry: self.registry(),
            accounts: s.accounts.clone(),
            costs: Arc::new(self.config.costs.clone()),
            risk: e.risk,
            evidence: Arc::new(StoreEvidenceLoader(s.evidence.clone())),
            lock: s.locks.clone(),
            clock: self.clock.clone(),
        };
        let settings = PaperSettings {
            account: self.config.file.account_id,
            initial_equity: paper.initial_equity,
            start: paper.start_date,
            close_time_utc: paper.close_time_utc,
            slippage_ticks: paper.slippage_ticks,
            warm_up_days: paper.warm_up_days,
            calendar_version: "unversioned".to_owned(),
        };
        PaperRunner::new(deps, settings).map(Some).map_err(error)
    }

    /// The advisory service for the effective settings, if AI is enabled.
    pub async fn ai_service(&self) -> Result<Option<AiService>, StoreError> {
        let e = self.effective().await?;
        if !e.ai.enabled {
            return Ok(None);
        }
        let mut advisors: Vec<Arc<dyn qd_ai::advisor::Advisor>> =
            vec![Arc::new(ChecklistAdvisor::new(e.ai.checklist.clone()))];
        for (provider, config, key_name) in [
            (
                qd_ai_providers::Provider::OpenAi,
                &e.ai.openai,
                "openai_api_key",
            ),
            (
                qd_ai_providers::Provider::Gemini,
                &e.ai.gemini,
                "gemini_api_key",
            ),
            (qd_ai_providers::Provider::Xai, &e.ai.xai, "xai_api_key"),
        ] {
            if !config.enabled {
                continue;
            }
            let Some(key) = self.secret(key_name).await else {
                tracing::warn!(
                    provider = provider.code(),
                    "advisor enabled without its API key; skipped"
                );
                continue;
            };
            match qd_ai_providers::ProviderAdvisor::new(
                provider,
                &config.model,
                provider.default_base(),
                key,
                std::time::Duration::from_secs(e.ai.timeout_seconds),
            ) {
                Ok(a) => advisors.push(Arc::new(a)),
                Err(err) => {
                    tracing::warn!(provider = provider.code(), error = %err, "advisor not built")
                }
            }
        }
        Ok(Some(AiService {
            orchestrator: AiOrchestrator::new(
                advisors,
                self.stores.journal.clone(),
                self.stores.journal.clone(),
                self.clock.clone(),
                AiSettings {
                    max_calls_per_day: e.ai.max_calls_per_day,
                    timeout_seconds: e.ai.timeout_seconds,
                },
            ),
            reader: self.stores.journal.clone(),
        }))
    }

    /// A secret's value, if stored and readable. Server-side adapters only.
    pub async fn secret(&self, name: &str) -> Option<String> {
        match self.secrets.get(name).await {
            Ok(value) => value.map(|v| v.expose().to_owned()),
            Err(e) => {
                tracing::warn!(secret = name, error = %e, "secret unreadable");
                None
            }
        }
    }

    /// A Kite client with today's session, for the effective settings.
    pub async fn kite_client(&self) -> Result<qd_broker_kite::client::KiteClient, StoreError> {
        let e = self.effective().await?;
        if !e.kite.enabled {
            return Err(StoreError(
                "the Zerodha connection is off (Settings → Zerodha Kite)".to_owned(),
            ));
        }
        let key = self.secret("kite_api_key").await.ok_or_else(|| {
            StoreError("the Kite API key is not set (Settings → API keys)".to_owned())
        })?;
        let token = self.secret("kite_access_token").await;
        qd_broker_kite::client::KiteClient::new(
            &self.kite_base,
            &key,
            token,
            std::time::Duration::from_secs(15),
        )
        .map_err(error)
    }

    /// The live runner, when `[live]` is configured.
    pub async fn live_runner(
        &self,
    ) -> Result<Option<qd_broker_kite::runner::LiveRunner>, StoreError> {
        let Some(live) = &self.config.file.live else {
            return Ok(None);
        };
        let e = self.effective().await?;
        let s = &self.stores;
        let deps = qd_broker_kite::runner::LiveDeps {
            journal: s.journal.clone(),
            reader: s.journal.clone(),
            halts: s.halts.clone(),
            market: s.market.clone(),
            registry: self.registry(),
            accounts: s.accounts.clone(),
            costs: Arc::new(self.config.costs.clone()),
            risk: e.risk,
            evidence: Arc::new(StoreEvidenceLoader(s.evidence.clone())),
            lock: s.locks.clone(),
            clock: self.clock.clone(),
            live: self.config.live,
        };
        let settings = qd_broker_kite::runner::LiveSettings {
            account: live.account_id,
            initial_equity: live.initial_equity,
            start: live.start_date,
            close_time_utc: live.close_time_utc,
            slippage_ticks: live.slippage_ticks,
            warm_up_days: live.warm_up_days,
            calendar_version: "unversioned".to_owned(),
        };
        qd_broker_kite::runner::LiveRunner::new(deps, settings)
            .map(Some)
            .map_err(error)
    }

    /// The daily paper run time, if paper trading is on and scheduled.
    pub async fn daily_run_utc(&self) -> Option<chrono::NaiveTime> {
        self.effective()
            .await
            .ok()
            .and_then(|e| e.paper)
            .and_then(|p| p.daily_run_utc)
    }
}

// ---------- services that read the effective settings on every call ----------

/// Paper trading with the current settings.
pub struct DynPaper(pub Arc<Runtime>);

const PAPER_OFF: &str = "paper trading is off (Settings → Paper trading)";

#[async_trait]
impl PaperTrading for DynPaper {
    async fn run_through(&self, through: NaiveDate) -> Result<Value, StoreError> {
        let runner = self
            .0
            .paper_runner()
            .await?
            .ok_or_else(|| StoreError(PAPER_OFF.to_owned()))?;
        runner.run_through(through).await
    }

    async fn state(&self) -> Result<Value, StoreError> {
        match self.0.paper_runner().await? {
            Some(runner) => runner.state().await,
            None => Ok(json!({ "configured": false })),
        }
    }
}

#[async_trait]
impl Reconciler for DynPaper {
    async fn reconcile(&self) -> Result<Vec<String>, StoreError> {
        match self.0.paper_runner().await? {
            Some(runner) => runner.reconcile().await,
            // No venue to reconcile against: entries stay halted (INV-07).
            None => Ok(vec![format!("{PAPER_OFF}; no venue to reconcile")]),
        }
    }
}

/// Advisory AI with the current settings.
pub struct DynAi(pub Arc<Runtime>);

const AI_OFF: &str = "advisory AI is disabled (Settings → Advisory AI)";

#[async_trait]
impl AiAdvisory for DynAi {
    async fn run(&self) -> Result<Value, StoreError> {
        let service = self
            .0
            .ai_service()
            .await?
            .ok_or_else(|| StoreError(AI_OFF.to_owned()))?;
        service.run().await
    }

    async fn advice(
        &self,
        decision: Option<qd_domain::ids::DecisionId>,
    ) -> Result<Value, StoreError> {
        // Reading past advice works even when AI is now disabled.
        qd_ai::service::AiService {
            orchestrator: AiOrchestrator::new(
                vec![],
                self.0.stores.journal.clone(),
                self.0.stores.journal.clone(),
                self.0.clock.clone(),
                AiSettings {
                    max_calls_per_day: 0,
                    timeout_seconds: 1,
                },
            ),
            reader: self.0.stores.journal.clone(),
        }
        .advice(decision)
        .await
    }

    async fn scorecard(&self) -> Result<Value, StoreError> {
        qd_ai::service::AiService {
            orchestrator: AiOrchestrator::new(
                vec![],
                self.0.stores.journal.clone(),
                self.0.stores.journal.clone(),
                self.0.clock.clone(),
                AiSettings {
                    max_calls_per_day: 0,
                    timeout_seconds: 1,
                },
            ),
            reader: self.0.stores.journal.clone(),
        }
        .scorecard()
        .await
    }
}

/// Validation with the current criteria and risk configuration.
pub struct DynValidator(pub Arc<Runtime>);

#[async_trait]
impl Validator for DynValidator {
    async fn validate(
        &self,
        request: &ValidationRequest,
        actor: &str,
    ) -> Result<Value, StoreError> {
        let rt = &self.0;
        let e = rt.effective().await?;
        qd_backtest::validator::StoreValidator::new(
            rt.stores.market.clone(),
            rt.registry(),
            rt.stores.evidence.clone(),
            rt.stores.audit.clone(),
            Arc::new(rt.config.costs.clone()),
            e.risk,
            e.validation,
            rt.clock.clone(),
        )?
        .validate(request, actor)
        .await
    }
}

/// Paper review with the current criteria.
pub struct DynReviewer(pub Arc<Runtime>);

impl DynReviewer {
    async fn reviewer(&self) -> Result<JournalReviewer, StoreError> {
        let rt = &self.0;
        Ok(JournalReviewer {
            reader: rt.stores.journal.clone(),
            evidence: rt.stores.evidence.clone(),
            audit: rt.stores.audit.clone(),
            clock: rt.clock.clone(),
            account: rt.config.file.account_id,
            criteria: rt.effective().await?.review,
        })
    }
}

#[async_trait]
impl Reviewer for DynReviewer {
    async fn review(&self) -> Result<Value, StoreError> {
        self.reviewer().await?.review().await
    }

    async fn record(
        &self,
        version: qd_domain::ids::StrategyVersionId,
        actor: &str,
    ) -> Result<Value, StoreError> {
        self.reviewer().await?.record(version, actor).await
    }
}

/// Walk-forward parameter searches with the current risk configuration.
pub struct DynSearch(pub Arc<Runtime>);

#[async_trait]
impl qd_app::ports::ParameterSearch for DynSearch {
    async fn search(&self, request: &qd_app::ports::SearchRequest) -> Result<Value, StoreError> {
        let rt = &self.0;
        qd_backtest::research::ResearchSearch::new(
            rt.stores.market.clone(),
            Arc::new(rt.config.costs.clone()),
            rt.effective().await?.risk,
            rt.clock.clone(),
        )
        .search(request)
        .await
    }
}

/// Research backtests with the current risk configuration.
pub struct DynBacktests(pub Arc<Runtime>);

#[async_trait]
impl BacktestRunner for DynBacktests {
    async fn run(&self, request: &BacktestRequest) -> Result<Value, StoreError> {
        let rt = &self.0;
        qd_backtest::research::ResearchBacktester::new(
            rt.stores.market.clone(),
            Arc::new(rt.config.costs.clone()),
            &rt.effective().await?.risk,
            rt.clock.clone(),
        )?
        .run(request)
        .await
    }
}

// ---------- settings administration ----------

/// Flattens nested objects into dotted keys, in the value's own order.
#[must_use]
pub fn flatten(value: &Value) -> Vec<(String, Value)> {
    fn walk(prefix: &str, v: &Value, out: &mut Vec<(String, Value)>) {
        match v {
            Value::Object(map) => {
                for (k, v) in map {
                    let key = if prefix.is_empty() {
                        k.clone()
                    } else {
                        format!("{prefix}.{k}")
                    };
                    walk(&key, v, out);
                }
            }
            other => out.push((prefix.to_owned(), other.clone())),
        }
    }
    let mut out = Vec::new();
    walk("", value, &mut out);
    out
}

fn set_path(root: &mut Value, key: &str, value: Value) {
    let mut node = root;
    let parts: Vec<&str> = key.split('.').collect();
    for (i, part) in parts.iter().enumerate() {
        let Some(map) = node.as_object_mut() else {
            return;
        };
        if i + 1 == parts.len() {
            map.insert((*part).to_owned(), value);
            return;
        }
        node = map
            .entry((*part).to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
    }
}

/// Fields whose value may be empty (null) and what they hold then.
const NULLABLE_KINDS: [(&str, &str); 2] = [("daily_run_utc", "time"), ("sync_bars_utc", "time")];

fn kind_of(key: &str, value: &Value) -> &'static str {
    let is_digits = |s: &str| s.chars().all(|c| c.is_ascii_digit());
    match value {
        Value::Bool(_) => "bool",
        Value::Number(_) => "integer",
        Value::String(s) => {
            let parts: Vec<&str> = s.split('-').collect();
            let colon: Vec<&str> = s.split(':').collect();
            if parts.len() == 3 && parts.iter().all(|p| !p.is_empty() && is_digits(p)) {
                "date"
            } else if (2..=3).contains(&colon.len())
                && colon.iter().all(|p| p.len() == 2 && is_digits(p))
            {
                "time"
            } else if s.parse::<rust_decimal::Decimal>().is_ok() {
                "decimal"
            } else {
                "text"
            }
        }
        _ => NULLABLE_KINDS
            .iter()
            .find(|(k, _)| key.ends_with(k))
            .map_or("text", |(_, kind)| kind),
    }
}

/// Labels and help for fields that need more than their name.
fn describe(section: &str, key: &str) -> (String, Option<&'static str>) {
    let help = match (section, key) {
        ("paper", "enabled") => {
            Some("Off: no paper runs, and entries stay halted at startup (no venue to reconcile).")
        }
        ("paper", "initial_equity") => Some("Starting equity in the account currency."),
        ("paper", "start_date") => Some("First trading date processed."),
        ("paper", "close_time_utc") => {
            Some("When a daily bar counts as complete (UTC). 10:00 UTC is 15:30 IST.")
        }
        ("paper", "daily_run_utc") => {
            Some("Automatic daily run time (UTC); empty means manual runs only.")
        }
        ("paper", "slippage_ticks") => Some("Adverse ticks on market and stop fills."),
        ("paper", "warm_up_days") => {
            Some("Calendar days of history loaded for indicators (250–3650).")
        }
        ("risk", "risk_per_trade") => Some("Fraction of equity risked per trade (0.01 = 1%)."),
        ("risk", "hard_halt_drawdown") => {
            Some("Drawdown from the peak that triggers a hard halt only you can re-arm.")
        }
        ("risk", "min_evidence") => Some("Comparable out-of-sample setups a decision needs."),
        ("risk", "min_ev_r") => Some("Minimum expected value after costs, in R."),
        ("risk", "stage_multipliers.paper") => {
            Some("Must stay 0: paper versions never trade live (INV-14).")
        }
        ("ai", "enabled") => {
            Some("Advice is journaled and scored only; it never changes a decision.")
        }
        ("ai", "max_calls_per_day") => Some("Per advisor, per UTC day; caps provider cost."),
        ("ai", "openai.enabled" | "gemini.enabled" | "xai.enabled") => Some(
            "Also needs the provider's API key. Sends the decision packet only: no equity, sizes or keys.",
        ),
        ("ai", "openai.model" | "gemini.model" | "xai.model") => {
            Some("Model name as the provider lists it; a new model is scored as a new advisor.")
        }
        ("kite", "enabled") => Some("Data import, daily login and fill checks at Zerodha."),
        ("kite", "user_id") => Some("Your Zerodha client id; a login as anyone else is refused."),
        ("kite", "market_protection") => Some(
            "Required on market and stop-market orders: -1 lets Zerodha choose, or 1 to 100 (%).",
        ),
        ("kite", "stop_limit_buffer") => Some(
            "The GTT stop leg is a limit order this far beyond the stop (0.01 = 1%), so it fills after a gap.",
        ),
        ("kite", "variety") => Some("auto: regular while the exchange is open, amo otherwise."),
        ("kite", "sync_bars_utc") => {
            Some("Daily bar import time (UTC); 11:00 UTC is 16:30 IST. Empty: manual only.")
        }
        ("kite", "history_days") => Some("Days of history fetched when an instrument has none."),
        ("kite", "sync_fills_minutes") => Some(
            "During market hours, fills are checked this often so protection goes out at once.",
        ),
        ("data", "max_close_jump") => Some(
            "A close this far from the previous one (0.20 = 20%) is suspect: a split, a bonus or a bad print.",
        ),
        ("data", "hold_suspect_bars") => Some(
            "Automatic imports stop at a suspect bar until you review it and upload the data yourself.",
        ),
        ("notifications", "enabled") => Some("Sends alerts and daily summaries to Telegram."),
        ("notifications", "telegram_chat_id") => Some(
            "Your chat id with the bot (send it a message, then read getUpdates), or @channel.",
        ),
        ("notifications", "min_severity") => Some("critical, or warning to also get warnings."),
        _ => None,
    };
    let label = key
        .split('.')
        .map(|p| {
            let words = p.replace('_', " ");
            let mut chars = words.chars();
            chars
                .next()
                .map(|c| c.to_uppercase().collect::<String>() + chars.as_str())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" › ");
    (label, help)
}

impl Runtime {
    fn section_view(
        &self,
        section: &str,
        saved: &HashMap<String, SettingVersion>,
    ) -> Result<Value, StoreError> {
        let (_, title, help) = SECTIONS
            .iter()
            .find(|(name, _, _)| *name == section)
            .ok_or_else(|| StoreError(format!("unknown settings section {section}")))?;
        let current = self.current(section, saved)?;
        let valid = validate_section(section, &current).err();
        let mut fields: Vec<Value> = flatten(&current)
            .into_iter()
            .map(|(key, value)| {
                let (label, help) = describe(section, &key);
                json!({ "key": key, "label": label, "help": help, "kind": kind_of(&key, &value), "value": value })
            })
            .collect();
        // Switches first.
        fields.sort_by_key(|f| f["key"] != "enabled");
        let source = match saved.get(section) {
            Some(v) if v.value.is_some() => json!({
                "kind": "saved", "version": v.version, "updated_by": v.updated_by, "updated_at": v.updated_at
            }),
            _ => json!({ "kind": "file" }),
        };
        Ok(json!({
            "name": section, "title": title, "help": help, "source": source,
            "fields": fields, "problem": valid,
        }))
    }
}

#[async_trait]
impl SettingsAdmin for Runtime {
    async fn view(&self) -> Result<Value, StoreError> {
        let saved = self.saved().await?;
        let sections: Vec<Value> = SECTIONS
            .iter()
            .map(|(name, _, _)| self.section_view(name, &saved))
            .collect::<Result<_, _>>()?;
        Ok(json!({
            "sections": sections,
            "server": {
                "environment": self.config.file.environment,
                "live_trading_enabled": self.config.live.live_trading_enabled,
                "live_orders_compiled": qd_app::live::live_orders_compiled(),
                "account_id": self.config.file.account_id,
                "note": "Environment, live trading, the account and the database are set on the server only, so no web session can turn on real money.",
            },
        }))
    }

    async fn update(
        &self,
        section: &str,
        values: &Map<String, Value>,
        actor: &str,
    ) -> Result<Value, SettingsError> {
        if !SECTIONS.iter().any(|(n, _, _)| *n == section) {
            return Err(SettingsError::Invalid(format!(
                "unknown settings section {section}"
            )));
        }
        if values.is_empty() {
            return Err(SettingsError::Invalid("nothing to change".to_owned()));
        }
        let saved = self.saved().await?;
        let before = self.current(section, &saved)?;
        let fields: HashMap<String, Value> = flatten(&before).into_iter().collect();
        let mut after = before.clone();
        let mut changes = Vec::new();
        for (key, incoming) in values {
            let old = fields
                .get(key)
                .ok_or_else(|| SettingsError::Invalid(format!("unknown field {key}")))?;
            let new = match (old, incoming) {
                // Empty input clears an optional field.
                (_, Value::String(s)) if s.trim().is_empty() => Value::Null,
                (Value::Number(_), Value::String(s)) => {
                    s.trim().parse::<i64>().map(Value::from).map_err(|_| {
                        SettingsError::Invalid(format!("{key} must be a whole number"))
                    })?
                }
                (Value::String(_) | Value::Null, Value::String(s)) => {
                    Value::String(s.trim().to_owned())
                }
                (Value::Bool(_), Value::Bool(b)) => Value::Bool(*b),
                (Value::Number(_), Value::Number(n)) => Value::Number(n.clone()),
                _ => {
                    return Err(SettingsError::Invalid(format!("{key} has the wrong type")));
                }
            };
            if &new != old {
                changes.push(json!({ "field": key, "from": old, "to": new }));
                set_path(&mut after, key, new);
            }
        }
        let normalized = validate_section(section, &after).map_err(SettingsError::Invalid)?;
        if changes.is_empty() {
            return self
                .section_view(section, &saved)
                .map_err(SettingsError::Store);
        }
        let version = self
            .stores
            .settings
            .put(section, Some(&normalized), actor)
            .await?;
        qd_app::ports::AuditLog::record(
            self.stores.audit.as_ref(),
            actor,
            "settings.update",
            json!({ "section": section, "version": version, "changes": changes }),
        )
        .await?;
        let saved = self.saved().await?;
        self.section_view(section, &saved)
            .map_err(SettingsError::Store)
    }

    async fn history(&self, section: &str) -> Result<Value, SettingsError> {
        if !SECTIONS.iter().any(|(n, _, _)| *n == section) {
            return Err(SettingsError::Invalid(format!(
                "unknown settings section {section}"
            )));
        }
        let versions = self.stores.settings.history(section).await?;
        let file_default = self.file_value(section)?;
        Ok(json!({
            "section": section,
            "file_default": file_default,
            "versions": versions.iter().map(|v| json!({
                "version": v.version,
                "value": v.value,
                "updated_by": v.updated_by,
                "updated_at": v.updated_at,
            })).collect::<Vec<_>>(),
        }))
    }

    async fn restore(
        &self,
        section: &str,
        version: i64,
        actor: &str,
    ) -> Result<Value, SettingsError> {
        let versions = self.stores.settings.history(section).await?;
        let old = versions
            .iter()
            .find(|v| v.version == version)
            .ok_or_else(|| SettingsError::Invalid(format!("{section} has no version {version}")))?;
        // A restored value must still be valid today (types or limits may
        // have changed since).
        let normalized = match &old.value {
            Some(value) => Some(validate_section(section, value).map_err(SettingsError::Invalid)?),
            None => None,
        };
        let new_version = self
            .stores
            .settings
            .put(section, normalized.as_ref(), actor)
            .await?;
        qd_app::ports::AuditLog::record(
            self.stores.audit.as_ref(),
            actor,
            "settings.restore",
            json!({ "section": section, "restored": version, "version": new_version }),
        )
        .await?;
        let saved = self.saved().await?;
        self.section_view(section, &saved)
            .map_err(SettingsError::Store)
    }

    async fn rotate_master_key(&self, actor: &str) -> Result<Value, SettingsError> {
        let count =
            crate::keys::rotate(&self.secrets, self.config.master_key_file.as_deref(), actor)
                .await
                .map_err(SettingsError::Invalid)?;
        Ok(json!({ "reencrypted": count }))
    }

    async fn reset(&self, section: &str, actor: &str) -> Result<Value, SettingsError> {
        if !SECTIONS.iter().any(|(n, _, _)| *n == section) {
            return Err(SettingsError::Invalid(format!(
                "unknown settings section {section}"
            )));
        }
        let version = self.stores.settings.put(section, None, actor).await?;
        qd_app::ports::AuditLog::record(
            self.stores.audit.as_ref(),
            actor,
            "settings.reset",
            json!({ "section": section, "version": version }),
        )
        .await?;
        let saved = self.saved().await?;
        self.section_view(section, &saved)
            .map_err(SettingsError::Store)
    }
}
