//! Server configuration (ADR 0007).
//!
//! Non-secret settings come from a TOML file; the database URL comes only from
//! the `QD_DATABASE_URL` environment variable and is never logged (INV-15).
//! Defaults are the safe ones: development environment, live trading disabled.
//! The server refuses to start with an inconsistent or unsafe configuration.

use std::path::{Path, PathBuf};

use qd_app::live::{Environment, LivePolicy, live_orders_compiled};
use qd_domain::costs::{CostScheduleData, CostScheduleSet, ScheduleCostModel};
use qd_domain::ids::AccountId;
use qd_risk::config::{RiskConfig, RiskConfigData};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A secret string. Its `Debug` output is redacted.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Wraps a secret.
    #[must_use]
    pub const fn new(value: String) -> Self {
        Self(value)
    }

    /// The secret value. Call only where it is used, never to log it.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(***)")
    }
}

fn default_bind() -> String {
    "127.0.0.1:8080".to_owned()
}

const fn default_max_connections() -> u32 {
    10
}

/// The configuration file.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    /// Deployment environment. Default: development.
    #[serde(default)]
    pub environment: Environment,
    /// HTTP bind address. Default: 127.0.0.1:8080 (local only).
    #[serde(default = "default_bind")]
    pub bind: String,
    /// Database pool size.
    #[serde(default = "default_max_connections")]
    pub database_max_connections: u32,
    /// Live trading switch. Default: false.
    #[serde(default)]
    pub live_trading_enabled: bool,
    /// The trading account this server operates.
    pub account_id: AccountId,
    /// Path to the risk configuration, relative to this file.
    pub risk_config: PathBuf,
    /// Paths to cost schedule files, relative to this file.
    pub cost_schedules: Vec<PathBuf>,
    /// Exchange calendars, relative to this file. Default
    /// `calendars/india.toml` (ADR 0015).
    #[serde(default = "default_calendars")]
    pub calendars: Vec<PathBuf>,
    /// Data-quality limits for bar imports (Settings → Data quality).
    #[serde(default)]
    pub data: DataConfig,
    /// Validation criteria, relative to this file. Default `validation.toml`.
    #[serde(default = "default_validation_criteria")]
    pub validation_criteria: PathBuf,
    /// Paper-review criteria, relative to this file. Default `review.toml`.
    #[serde(default = "default_review_criteria")]
    pub review_criteria: PathBuf,
    /// Built frontend directory to serve, relative to this file. Optional.
    #[serde(default)]
    pub frontend_dir: Option<PathBuf>,
    /// Session lifetime in hours. Default: 12.
    #[serde(default = "default_session_hours")]
    pub session_hours: i64,
    /// Writable directory for server state (the generated master key),
    /// relative to this file. Optional (ADR 0013).
    #[serde(default)]
    pub data_dir: Option<PathBuf>,
    /// Paper trading. Absent: no paper runner, and entries stay halted
    /// because no venue can be reconciled.
    #[serde(default)]
    pub paper: Option<PaperConfig>,
    /// Advisory AI (INV-04). Disabled by default.
    #[serde(default)]
    pub ai: AiConfig,
    /// Zerodha Kite connection (ADR 0014). Disabled by default.
    #[serde(default)]
    pub kite: KiteConfig,
    /// Telegram notifications (ADR 0014). Disabled by default.
    #[serde(default)]
    pub notifications: NotificationsConfig,
    /// Live trading at Zerodha (ADR 0014). Absent: no live runner. Server
    /// file only, never editable from the web UI (ADR 0013).
    #[serde(default)]
    pub live: Option<LiveConfig>,
}

/// One hosted-model advisor.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    /// Whether the advisor runs (it also needs its API key).
    pub enabled: bool,
    /// Model name, as the provider lists it.
    pub model: String,
}

impl ProviderConfig {
    fn off(model: &str) -> Self {
        Self {
            enabled: false,
            model: model.to_owned(),
        }
    }
}

fn default_openai() -> ProviderConfig {
    ProviderConfig::off("gpt-6-astra")
}

fn default_gemini() -> ProviderConfig {
    ProviderConfig::off("gemini-3.8-flash")
}

fn default_xai() -> ProviderConfig {
    ProviderConfig::off("grok-4.7")
}

fn default_sync_bars_utc() -> Option<chrono::NaiveTime> {
    // 16:30 IST, after the NSE close and its closing data.
    chrono::NaiveTime::from_hms_opt(11, 0, 0)
}

const fn default_history_days() -> i64 {
    600
}

const fn default_sync_fills_minutes() -> u32 {
    5
}

const fn default_market_protection() -> i32 {
    -1
}

fn default_stop_limit_buffer() -> rust_decimal::Decimal {
    rust_decimal::Decimal::new(1, 2)
}

/// Zerodha Kite connection settings (Settings → Zerodha Kite).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct KiteConfig {
    /// Master switch for the connection (data, login, fills).
    #[serde(default)]
    pub enabled: bool,
    /// The Zerodha client id a login must belong to.
    #[serde(default)]
    pub user_id: String,
    /// `market_protection` on MARKET and SL-M orders: -1 (automatic) or 1–100.
    #[serde(default = "default_market_protection")]
    pub market_protection: i32,
    /// The GTT stop leg's limit sits this fraction beyond the stop.
    #[serde(
        default = "default_stop_limit_buffer",
        with = "rust_decimal::serde::str"
    )]
    pub stop_limit_buffer: rust_decimal::Decimal,
    /// `auto`, `regular` or `amo`.
    #[serde(default)]
    pub variety: qd_broker_kite::broker::VarietyChoice,
    /// Daily bar import time (UTC); empty means manual only.
    #[serde(default = "default_sync_bars_utc")]
    pub sync_bars_utc: Option<chrono::NaiveTime>,
    /// Calendar days of history requested on the first import.
    #[serde(default = "default_history_days")]
    pub history_days: i64,
    /// Minutes between fill checks during market hours (live only).
    #[serde(default = "default_sync_fills_minutes")]
    pub sync_fills_minutes: u32,
}

impl Default for KiteConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            user_id: String::new(),
            market_protection: default_market_protection(),
            stop_limit_buffer: default_stop_limit_buffer(),
            variety: qd_broker_kite::broker::VarietyChoice::Auto,
            sync_bars_utc: default_sync_bars_utc(),
            history_days: default_history_days(),
            sync_fills_minutes: default_sync_fills_minutes(),
        }
    }
}

impl KiteConfig {
    /// The order settings.
    #[must_use]
    pub const fn orders(&self) -> qd_broker_kite::broker::OrderSettings {
        qd_broker_kite::broker::OrderSettings {
            market_protection: self.market_protection,
            stop_limit_buffer: self.stop_limit_buffer,
            variety: self.variety,
        }
    }

    /// Range checks.
    pub fn validate(&self) -> Result<(), String> {
        self.orders().validate()?;
        if !(30..=3650).contains(&self.history_days) {
            return Err("kite.history_days must be between 30 and 3650".to_owned());
        }
        if !(1..=60).contains(&self.sync_fills_minutes) {
            return Err("kite.sync_fills_minutes must be between 1 and 60".to_owned());
        }
        if self.user_id.len() > 16 || !self.user_id.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(
                "kite.user_id must be your Zerodha client id (letters and digits)".to_owned(),
            );
        }
        if self.enabled && self.user_id.is_empty() {
            return Err("kite.user_id is required when the connection is enabled".to_owned());
        }
        Ok(())
    }
}

fn default_min_severity() -> String {
    "critical".to_owned()
}

const fn default_true() -> bool {
    true
}

/// Telegram notifications (Settings → Notifications).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NotificationsConfig {
    /// Master switch.
    #[serde(default)]
    pub enabled: bool,
    /// The Telegram chat id messages go to.
    #[serde(default)]
    pub telegram_chat_id: String,
    /// `critical` or `warning`: the least severe alert sent.
    #[serde(default = "default_min_severity")]
    pub min_severity: String,
    /// A summary after each daily paper or live run.
    #[serde(default = "default_true")]
    pub daily_summary: bool,
}

impl Default for NotificationsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            telegram_chat_id: String::new(),
            min_severity: default_min_severity(),
            daily_summary: true,
        }
    }
}

impl NotificationsConfig {
    /// Range checks.
    pub fn validate(&self) -> Result<(), String> {
        if !matches!(self.min_severity.as_str(), "critical" | "warning") {
            return Err("notifications.min_severity must be critical or warning".to_owned());
        }
        let id = self
            .telegram_chat_id
            .strip_prefix('-')
            .unwrap_or(&self.telegram_chat_id);
        let valid_id = (!id.is_empty() && id.chars().all(|c| c.is_ascii_digit()))
            || (id.starts_with('@') && id.len() > 1);
        if !self.telegram_chat_id.is_empty() && !valid_id {
            return Err(
                "notifications.telegram_chat_id must be a numeric chat id or @channel".to_owned(),
            );
        }
        if self.enabled && self.telegram_chat_id.is_empty() {
            return Err(
                "notifications.telegram_chat_id is required when notifications are enabled"
                    .to_owned(),
            );
        }
        Ok(())
    }
}

/// Live trading at Zerodha (server file only).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LiveConfig {
    /// The live account (mode `live`).
    pub account_id: AccountId,
    /// Capital the live book starts from, in the account currency.
    #[serde(with = "rust_decimal::serde::str")]
    pub initial_equity: rust_decimal::Decimal,
    /// First trading date the live book processes.
    pub start_date: chrono::NaiveDate,
    /// When a date's daily bar counts as complete (UTC). Default 10:00.
    #[serde(default = "default_close_time_utc")]
    pub close_time_utc: chrono::NaiveTime,
    /// Stop-slippage assumption for decisions, in ticks. Default 1.
    #[serde(default = "default_slippage_ticks", with = "rust_decimal::serde::str")]
    pub slippage_ticks: rust_decimal::Decimal,
    /// Calendar days of history for indicators. Default 400.
    #[serde(default = "default_warm_up_days")]
    pub warm_up_days: i64,
    /// Time (UTC) of the automatic daily live run; absent means manual only.
    #[serde(default)]
    pub daily_run_utc: Option<chrono::NaiveTime>,
}

const fn default_ai_calls() -> u32 {
    200
}

const fn default_ai_timeout() -> u64 {
    20
}

/// Advisory AI settings (ADR 0011). Advice is shadow-only (INV-04).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AiConfig {
    /// Master switch. Default: false.
    #[serde(default)]
    pub enabled: bool,
    /// Calls per advisor per UTC day. Default 200.
    #[serde(default = "default_ai_calls")]
    pub max_calls_per_day: u32,
    /// Timeout per call in seconds. Default 20.
    #[serde(default = "default_ai_timeout")]
    pub timeout_seconds: u64,
    /// Thresholds of the deterministic checklist advisor.
    #[serde(default)]
    pub checklist: qd_ai::checklist::ChecklistConfig,
    /// OpenAI advisor (key: `openai_api_key`).
    #[serde(default = "default_openai")]
    pub openai: ProviderConfig,
    /// Gemini advisor (key: `gemini_api_key`).
    #[serde(default = "default_gemini")]
    pub gemini: ProviderConfig,
    /// xAI advisor (key: `xai_api_key`).
    #[serde(default = "default_xai")]
    pub xai: ProviderConfig,
}

impl AiConfig {
    /// Range checks.
    pub fn validate(&self) -> Result<(), String> {
        if self.timeout_seconds == 0 || self.timeout_seconds > 300 {
            return Err("ai.timeout_seconds must be between 1 and 300".to_owned());
        }
        if self.max_calls_per_day > 100_000 {
            return Err("ai.max_calls_per_day must be at most 100000".to_owned());
        }
        for (name, p) in [
            ("openai", &self.openai),
            ("gemini", &self.gemini),
            ("xai", &self.xai),
        ] {
            let model = p.model.trim();
            if model.is_empty()
                || model.len() > 100
                || !model
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-._:/".contains(c))
                || model.contains("..")
            {
                return Err(format!(
                    "ai.{name}.model must be a model name such as the provider lists"
                ));
            }
        }
        Ok(())
    }
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_calls_per_day: default_ai_calls(),
            timeout_seconds: default_ai_timeout(),
            checklist: qd_ai::checklist::ChecklistConfig::default(),
            openai: default_openai(),
            gemini: default_gemini(),
            xai: default_xai(),
        }
    }
}

fn default_close_time_utc() -> chrono::NaiveTime {
    // 15:30 IST, the NSE close, is 10:00 UTC.
    chrono::NaiveTime::from_hms_opt(10, 0, 0).unwrap_or(chrono::NaiveTime::MIN)
}

const fn default_slippage_ticks() -> rust_decimal::Decimal {
    rust_decimal::Decimal::ONE
}

const fn default_warm_up_days() -> i64 {
    400
}

/// Paper-trading settings (ADR 0009).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PaperConfig {
    /// Starting equity in the account currency.
    #[serde(with = "rust_decimal::serde::str")]
    pub initial_equity: rust_decimal::Decimal,
    /// First trading date to process.
    pub start_date: chrono::NaiveDate,
    /// When a date's daily bar counts as complete (UTC). Default 10:00 (15:30 IST).
    #[serde(default = "default_close_time_utc")]
    pub close_time_utc: chrono::NaiveTime,
    /// Adverse slippage on market and stop fills, in ticks. Default 1.
    #[serde(default = "default_slippage_ticks", with = "rust_decimal::serde::str")]
    pub slippage_ticks: rust_decimal::Decimal,
    /// Calendar days of history loaded for indicator warm-up. Default 400.
    #[serde(default = "default_warm_up_days")]
    pub warm_up_days: i64,
    /// Time (UTC) of the automatic daily run; absent means runs are manual only.
    #[serde(default)]
    pub daily_run_utc: Option<chrono::NaiveTime>,
}

fn default_calendars() -> Vec<PathBuf> {
    vec![PathBuf::from("calendars/india.toml")]
}

const fn default_true_data() -> bool {
    true
}

/// Data-quality limits for bar imports (ADR 0015).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DataConfig {
    /// A close more than this fraction from the previous close is suspect.
    #[serde(with = "rust_decimal::serde::str")]
    pub max_close_jump: rust_decimal::Decimal,
    /// Hold back suspect bars (and every later one) from automatic imports
    /// until you review them.
    #[serde(default = "default_true_data")]
    pub hold_suspect_bars: bool,
}

impl Default for DataConfig {
    fn default() -> Self {
        Self {
            max_close_jump: qd_domain::calendar::QualityLimits::default().max_close_jump,
            hold_suspect_bars: true,
        }
    }
}

impl DataConfig {
    /// Range checks.
    pub fn validate(&self) -> Result<(), String> {
        if self.max_close_jump < rust_decimal::Decimal::new(5, 2)
            || self.max_close_jump > rust_decimal::Decimal::ONE
        {
            return Err("data.max_close_jump must be between 0.05 and 1".to_owned());
        }
        Ok(())
    }

    /// The limits.
    #[must_use]
    pub const fn limits(&self) -> qd_domain::calendar::QualityLimits {
        qd_domain::calendar::QualityLimits {
            max_close_jump: self.max_close_jump,
        }
    }
}

fn default_validation_criteria() -> PathBuf {
    PathBuf::from("validation.toml")
}

fn default_review_criteria() -> PathBuf {
    PathBuf::from("review.toml")
}

const fn default_session_hours() -> i64 {
    12
}

/// Why the configuration was refused.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// A file could not be read.
    #[error("cannot read {path}: {source}")]
    Read {
        /// The path.
        path: PathBuf,
        /// The error.
        source: std::io::Error,
    },
    /// A file could not be parsed.
    #[error("cannot parse {path}: {detail}")]
    Parse {
        /// The path.
        path: PathBuf,
        /// What is wrong.
        detail: String,
    },
    /// A required environment variable is missing.
    #[error("environment variable {0} is not set")]
    MissingEnv(&'static str),
    /// The settings are unsafe or contradictory.
    #[error("refusing to start: {0}")]
    Unsafe(String),
}

/// The validated server configuration.
#[derive(Clone, Debug)]
pub struct ServerConfig {
    /// The file settings.
    pub file: ConfigFile,
    /// Database URL (secret).
    pub database_url: Secret,
    /// Risk configuration.
    pub risk: RiskConfig,
    /// Raw cost schedules (validated into the model below).
    pub cost_schedules: Vec<CostScheduleData>,
    /// The shared cost model.
    pub costs: ScheduleCostModel,
    /// Live-trading policy.
    pub live: LivePolicy,
    /// Validation criteria (ADR 0010).
    pub validation: qd_backtest::validation::ValidationCriteria,
    /// Paper-review criteria (ADR 0010).
    pub review: qd_app::review::ReviewCriteria,
    /// Master key for the secrets store (ADR 0013); `None` disables it.
    pub master_key: Option<qd_store::settings::MasterKey>,
    /// Exchange calendars (ADR 0015).
    pub calendars: qd_domain::calendar::Calendars,
    /// Directory the config file is in (relative paths resolve against it).
    pub base_dir: PathBuf,
}

fn read(path: &Path) -> Result<String, ConfigError> {
    std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.to_owned(),
        source,
    })
}

fn parse<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, ConfigError> {
    toml::from_str(&read(path)?).map_err(|e| ConfigError::Parse {
        path: path.to_owned(),
        detail: e.to_string(),
    })
}

impl PaperConfig {
    /// Range checks.
    pub fn validate(&self) -> Result<(), String> {
        if self.initial_equity <= rust_decimal::Decimal::ZERO
            || self.slippage_ticks < rust_decimal::Decimal::ZERO
            || !(250..=3650).contains(&self.warm_up_days)
        {
            return Err(
                "paper: initial_equity must be positive, slippage_ticks non-negative, \
                 warm_up_days between 250 and 3650"
                    .to_owned(),
            );
        }
        Ok(())
    }
}

/// Loads the secrets master key: `QD_MASTER_KEY` (64 hex characters) wins;
/// otherwise `<data_dir>/master.key`, generated on first start with mode
/// 0600. With neither, the secrets store is disabled.
pub fn load_master_key(
    env_value: Option<String>,
    data_dir: Option<&Path>,
) -> Result<Option<qd_store::settings::MasterKey>, ConfigError> {
    let parse_hex = |text: &str, source: &str| {
        let bytes = hex::decode(text.trim()).map_err(|_| {
            ConfigError::Unsafe(format!("{source} must be 64 hexadecimal characters"))
        })?;
        let key: [u8; 32] = bytes.try_into().map_err(|_| {
            ConfigError::Unsafe(format!("{source} must be 64 hexadecimal characters"))
        })?;
        Ok(Some(qd_store::settings::MasterKey::new(key)))
    };
    if let Some(value) = env_value.filter(|v| !v.trim().is_empty()) {
        return parse_hex(&value, "QD_MASTER_KEY");
    }
    let Some(dir) = data_dir else {
        return Ok(None);
    };
    let path = dir.join("master.key");
    match std::fs::read_to_string(&path) {
        Ok(text) => parse_hex(&text, "the master key file"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(dir).map_err(|source| ConfigError::Read {
                path: dir.to_owned(),
                source,
            })?;
            let mut key = [0_u8; 32];
            getrandom::fill(&mut key).map_err(|e| {
                ConfigError::Unsafe(format!("no randomness for the master key: {e}"))
            })?;
            write_private(&path, hex::encode(key).as_bytes())?;
            Ok(Some(qd_store::settings::MasterKey::new(key)))
        }
        Err(source) => Err(ConfigError::Read { path, source }),
    }
}

/// Writes a new file readable only by its owner; refuses to overwrite.
fn write_private(path: &Path, contents: &[u8]) -> Result<(), ConfigError> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|source| ConfigError::Read {
        path: path.to_owned(),
        source,
    })?;
    file.write_all(contents)
        .map_err(|source| ConfigError::Read {
            path: path.to_owned(),
            source,
        })
}

impl ServerConfig {
    /// Loads and validates the configuration. `env` reads environment variables.
    pub fn load(path: &Path, env: &dyn Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let file: ConfigFile = parse(path)?;
        let base = path.parent().unwrap_or_else(|| Path::new("."));
        let risk_data: RiskConfigData = parse(&base.join(&file.risk_config))?;
        let risk = RiskConfig::new(risk_data).map_err(|e| ConfigError::Parse {
            path: file.risk_config.clone(),
            detail: e.to_string(),
        })?;
        let mut cost_schedules = Vec::new();
        for schedule_path in &file.cost_schedules {
            let set: CostScheduleSet = parse(&base.join(schedule_path))?;
            cost_schedules.extend(set.schedules);
        }
        let costs = ScheduleCostModel::new(CostScheduleSet {
            schedules: cost_schedules.clone(),
        })
        .map_err(|e| ConfigError::Parse {
            path: base.to_owned(),
            detail: e.to_string(),
        })?;
        let validation: qd_backtest::validation::ValidationCriteria =
            parse(&base.join(&file.validation_criteria))?;
        validation.validate().map_err(|detail| ConfigError::Parse {
            path: file.validation_criteria.clone(),
            detail,
        })?;
        let review: qd_app::review::ReviewCriteria = parse(&base.join(&file.review_criteria))?;
        review.validate().map_err(|detail| ConfigError::Parse {
            path: file.review_criteria.clone(),
            detail,
        })?;
        let database_url = env("QD_DATABASE_URL")
            .filter(|v| !v.trim().is_empty())
            .map(Secret::new)
            .ok_or(ConfigError::MissingEnv("QD_DATABASE_URL"))?;
        let live = LivePolicy {
            environment: file.environment,
            live_trading_enabled: file.live_trading_enabled,
        };
        if let Some(paper) = &file.paper {
            paper.validate().map_err(ConfigError::Unsafe)?;
        }
        file.ai.validate().map_err(ConfigError::Unsafe)?;
        file.kite.validate().map_err(ConfigError::Unsafe)?;
        file.data.validate().map_err(ConfigError::Unsafe)?;
        let mut calendar_data = Vec::new();
        for calendar_path in &file.calendars {
            let set: qd_domain::calendar::CalendarSet = parse(&base.join(calendar_path))?;
            calendar_data.extend(set.calendars);
        }
        let calendars = qd_domain::calendar::Calendars::new(qd_domain::calendar::CalendarSet {
            calendars: calendar_data,
        })
        .map_err(|e| ConfigError::Parse {
            path: base.to_owned(),
            detail: e.to_string(),
        })?;
        file.notifications.validate().map_err(ConfigError::Unsafe)?;
        if let Some(live) = &file.live {
            if live.initial_equity <= rust_decimal::Decimal::ZERO
                || live.slippage_ticks < rust_decimal::Decimal::ZERO
                || !(250..=3650).contains(&live.warm_up_days)
            {
                return Err(ConfigError::Unsafe(
                    "live: initial_equity must be positive, slippage_ticks non-negative, \
                     warm_up_days between 250 and 3650"
                        .to_owned(),
                ));
            }
            if live.account_id == file.account_id {
                return Err(ConfigError::Unsafe(
                    "live.account_id must differ from the paper account_id".to_owned(),
                ));
            }
        }
        let master_key = load_master_key(
            env("QD_MASTER_KEY"),
            file.data_dir.as_ref().map(|d| base.join(d)).as_deref(),
        )?;
        if !(1..=168).contains(&file.session_hours) {
            return Err(ConfigError::Unsafe(
                "session_hours must be between 1 and 168".to_owned(),
            ));
        }
        let config = Self {
            file,
            database_url,
            risk,
            cost_schedules,
            costs,
            live,
            validation,
            review,
            master_key,
            calendars,
            base_dir: base.to_owned(),
        };
        config.validate()?;
        Ok(config)
    }

    /// Refuses unsafe or contradictory settings.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.live.live_trading_enabled {
            if self.live.environment != Environment::Production {
                return Err(ConfigError::Unsafe(
                    "live_trading_enabled requires environment = \"production\"".to_owned(),
                ));
            }
            if !live_orders_compiled() {
                return Err(ConfigError::Unsafe(
                    "live_trading_enabled requires a build with the live-orders feature".to_owned(),
                ));
            }
            if self.costs.schedules().iter().any(|s| !s.is_verified()) {
                return Err(ConfigError::Unsafe(
                    "live trading requires every cost schedule to be verified".to_owned(),
                ));
            }
        }
        Ok(())
    }
}
