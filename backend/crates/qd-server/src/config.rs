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
        let database_url = env("QD_DATABASE_URL")
            .filter(|v| !v.trim().is_empty())
            .map(Secret::new)
            .ok_or(ConfigError::MissingEnv("QD_DATABASE_URL"))?;
        let live = LivePolicy {
            environment: file.environment,
            live_trading_enabled: file.live_trading_enabled,
        };
        let config = Self {
            file,
            database_url,
            risk,
            cost_schedules,
            costs,
            live,
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
