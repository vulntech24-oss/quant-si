//! Agent settings, identity and the live gate (ADR 0016).

use qd_app::decision::StrategyVersionInfo;
use qd_domain::economics::SlippageAssumption;
use qd_domain::ids::{StrategyId, StrategyVersionId};
use qd_domain::instrument::InstrumentSpec;
use qd_domain::lifecycle::strategy::StrategyStage;
use qd_domain::proposal::StrategyRef;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::predictions::Scorecard;

/// Version of the agent's instructions and tools; part of its identity.
pub const PROMPT_VERSION: &str = "agent-v1";

/// Budgets and limits of the agent (web UI settings, `agent` section).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentSettings {
    /// Model turns per run.
    pub max_steps: u32,
    /// Tool calls per run.
    pub max_tool_calls: u32,
    /// Trade requests per run.
    pub max_trades_per_run: u32,
    /// Wall-clock limit of a run, seconds.
    pub run_timeout_secs: u64,
    /// Net reward-to-risk floor of agent plans (the Risk Gate enforces it).
    pub min_rr: Decimal,
    /// Largest entry notional as a fraction of equity (a deterministic cap
    /// on the agent's allocation, applied before the Risk Gate).
    pub max_position_fraction: Decimal,
    /// Stop-slippage assumption, in ticks.
    pub slippage_ticks: Decimal,
    /// Longest holding period the agent may plan, trading days.
    pub max_holding_days: u16,
    /// The owner's standing guidance: focus, universe, style.
    pub instructions: String,
}

impl Default for AgentSettings {
    fn default() -> Self {
        Self {
            max_steps: 24,
            max_tool_calls: 40,
            max_trades_per_run: 3,
            run_timeout_secs: 600,
            min_rr: Decimal::new(15, 1),
            max_position_fraction: Decimal::new(25, 2),
            slippage_ticks: Decimal::ONE,
            max_holding_days: 60,
            instructions: String::new(),
        }
    }
}

impl AgentSettings {
    /// Checks the values.
    pub fn validate(&self) -> Result<(), String> {
        let ok = self.max_steps >= 1
            && self.max_steps <= 100
            && self.max_tool_calls >= 1
            && self.max_tool_calls <= 200
            && self.max_trades_per_run <= 10
            && (30..=3600).contains(&self.run_timeout_secs)
            && self.min_rr >= Decimal::ONE
            && self.max_position_fraction > Decimal::ZERO
            && self.max_position_fraction <= Decimal::ONE
            && self.slippage_ticks >= Decimal::ZERO
            && (1..=120).contains(&self.max_holding_days)
            && self.instructions.len() <= 4000;
        if ok {
            Ok(())
        } else {
            Err("agent settings are out of range".to_owned())
        }
    }
}

/// When the agent may trade the live book (server file `[agent.live]`,
/// never the web UI). Every threshold is a deterministic check on the
/// agent's scored paper record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LiveGate {
    /// The owner allows the agent on the live book at all.
    pub enabled: bool,
    /// Scored predictions needed.
    pub min_evaluated_predictions: u32,
    /// Accuracy needed.
    pub min_accuracy: Decimal,
    /// Brier score at most.
    pub max_brier: Decimal,
    /// Closed paper trades needed.
    pub min_trades: u32,
}

impl Default for LiveGate {
    fn default() -> Self {
        Self {
            enabled: false,
            min_evaluated_predictions: 30,
            min_accuracy: Decimal::new(55, 2),
            max_brier: Decimal::new(25, 2),
            min_trades: 20,
        }
    }
}

impl LiveGate {
    /// The stage the agent trades a book at. Paper books: Paper. The live
    /// book: SmallCapital only when the owner enabled it and the record
    /// passes every threshold; otherwise Paper, which the Risk Gate refuses
    /// on a live account (and the Order Gateway would too, INV-14).
    #[must_use]
    pub fn stage(&self, live_book: bool, record: &Scorecard) -> (StrategyStage, Vec<String>) {
        if !live_book {
            return (StrategyStage::Paper, Vec::new());
        }
        let mut missing = Vec::new();
        if !self.enabled {
            missing.push("the agent is not enabled for live trading in the server file".to_owned());
        }
        if record.evaluated < self.min_evaluated_predictions {
            missing.push(format!(
                "{} of {} scored predictions",
                record.evaluated, self.min_evaluated_predictions
            ));
        }
        if record.accuracy.is_none_or(|a| a < self.min_accuracy) {
            missing.push(format!("accuracy below {}", self.min_accuracy));
        }
        if record.brier.is_none_or(|b| b > self.max_brier) {
            missing.push(format!("Brier score above {}", self.max_brier));
        }
        if record.trades < self.min_trades || record.trade_net_pnl <= Decimal::ZERO {
            missing.push(format!(
                "{} of {} closed trades with a positive net P&L",
                record.trades, self.min_trades
            ));
        }
        if missing.is_empty() {
            (StrategyStage::SmallCapital, missing)
        } else {
            (StrategyStage::Paper, missing)
        }
    }
}

/// A stable UUID from a label (SHA-256, RFC 9562 version 8 layout).
fn stable_uuid(label: &str) -> uuid::Uuid {
    let digest = Sha256::digest(label.as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(bytes)
}

/// The agent's identity as a strategy version: one version per model and
/// prompt version, so each model's record is kept apart.
#[must_use]
pub fn agent_ref(model: &str) -> StrategyRef {
    let logic_version = format!("{PROMPT_VERSION}:{model}");
    StrategyRef {
        strategy_id: StrategyId::from_uuid(stable_uuid("quantdesk-ai-agent")),
        name: "AI agent".to_owned(),
        version_id: StrategyVersionId::from_uuid(stable_uuid(&logic_version)),
        version_number: 1,
        logic_version,
        git_sha: option_env!("QD_GIT_SHA").unwrap_or("unknown").to_owned(),
    }
}

/// The agent as the Decision Engine sees it for one instrument.
pub fn version_info(
    model: &str,
    stage: StrategyStage,
    settings: &AgentSettings,
    spec: &InstrumentSpec,
) -> Result<StrategyVersionInfo, String> {
    let slippage =
        SlippageAssumption::new("slip-ticks-v1", settings.slippage_ticks * spec.tick_size)
            .map_err(|e| e.to_string())?;
    Ok(StrategyVersionInfo {
        reference: agent_ref(model),
        stage,
        rr_floor: settings.min_rr,
        slippage,
    })
}
