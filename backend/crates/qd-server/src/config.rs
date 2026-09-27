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
use serde::Deserialize;
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
    /// Paper trading. Absent: no paper runner, and entries stay halted
    /// because no venue can be reconciled.
    #[serde(default)]
    pub paper: Option<PaperConfig>,
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
#[derive(Clone, Debug, Deserialize)]
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
        let database_url = env("QD_DATABASE_URL")
            .filter(|v| !v.trim().is_empty())
            .map(Secret::new)
            .ok_or(ConfigError::MissingEnv("QD_DATABASE_URL"))?;
        let live = LivePolicy {
            environment: file.environment,
            live_trading_enabled: file.live_trading_enabled,
        };
        if let Some(paper) = &file.paper {
            if paper.initial_equity <= rust_decimal::Decimal::ZERO
                || paper.slippage_ticks < rust_decimal::Decimal::ZERO
                || !(250..=3650).contains(&paper.warm_up_days)
            {
                return Err(ConfigError::Unsafe(
                    "paper: initial_equity must be positive, slippage_ticks non-negative, \
                     warm_up_days between 250 and 3650"
                        .to_owned(),
                ));
            }
        }
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
