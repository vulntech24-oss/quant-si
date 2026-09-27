//! A deterministic checklist advisor: no network, no model, same input →
//! same advice. It flags the things a careful owner checks before agreeing
//! with a trade. It is the first advisor so the whole advisory path runs
//! without any provider, and its stances are scored like any other's.

use async_trait::async_trait;
use qd_app::journal::AiStance;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::advisor::{AdviceDraft, Advisor, AdvisorError, AdvisorInput};

/// Checklist thresholds (`[ai.checklist]` in the server configuration).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChecklistConfig {
    /// Net RR below this is flagged as barely clearing the floor.
    pub min_comfortable_rr: Decimal,
    /// EV below this (in R) is flagged as thin.
    pub min_comfortable_ev_r: Decimal,
    /// Fewer comparable setups than this is flagged as thin evidence.
    pub min_comfortable_evidence: u32,
    /// A stop farther than this fraction of the entry is flagged as wide.
    pub max_stop_distance: Decimal,
    /// Costs above this share of the net reward are flagged.
    pub max_cost_share_of_reward: Decimal,
}

impl Default for ChecklistConfig {
    fn default() -> Self {
        Self {
            min_comfortable_rr: Decimal::new(20, 1),
            min_comfortable_ev_r: Decimal::new(2, 1),
            min_comfortable_evidence: 60,
            max_stop_distance: Decimal::new(10, 2),
            max_cost_share_of_reward: Decimal::new(20, 2),
        }
    }
}

/// The checklist advisor.
#[derive(Clone, Debug, Default)]
pub struct ChecklistAdvisor {
    config: ChecklistConfig,
}

impl ChecklistAdvisor {
    /// Advisor name and version.
    pub const NAME: &'static str = "checklist-v1";

    /// Creates the advisor.
    #[must_use]
    pub const fn new(config: ChecklistConfig) -> Self {
        Self { config }
    }

    /// The flags for one decision, in a fixed order.
    #[must_use]
    pub fn flags(&self, input: &AdvisorInput) -> Vec<String> {
        let c = &self.config;
        let mut flags = Vec::new();
        if input.rr_net.is_some_and(|rr| rr < c.min_comfortable_rr) {
            flags.push(format!(
                "Reward-to-risk {} is below {}: little room if costs or slippage run high.",
                input.rr_net.unwrap_or_default().round_dp(2),
                c.min_comfortable_rr
            ));
        }
        if input.ev_r.is_some_and(|ev| ev < c.min_comfortable_ev_r) {
            flags.push(format!(
                "Expected value {}R is thin; a small calibration error erases it.",
                input.ev_r.unwrap_or_default().round_dp(3)
            ));
        }
        if input
            .evidence_count
            .is_some_and(|n| n < c.min_comfortable_evidence)
        {
            flags.push(format!(
                "Probabilities rest on {} comparable setups, fewer than {}.",
                input.evidence_count.unwrap_or_default(),
                c.min_comfortable_evidence
            ));
        }
        if let (Some(entry), Some(stop)) = (input.entry, input.stop) {
            if entry > Decimal::ZERO && ((entry - stop).abs() / entry) > c.max_stop_distance {
                flags.push(format!(
                    "The stop is {}% from the entry: a gap through it costs more than planned.",
                    ((entry - stop).abs() / entry * Decimal::ONE_HUNDRED).round_dp(1)
                ));
            }
        }
        if let (Some(costs), Some(reward)) = (input.costs_per_unit, input.reward_net) {
            if reward > Decimal::ZERO && costs / reward > c.max_cost_share_of_reward {
                flags.push(format!(
                    "Costs take {}% of the net reward.",
                    (costs / reward * Decimal::ONE_HUNDRED).round_dp(1)
                ));
            }
        }
        if input.regime.as_deref() == Some("high_volatility") {
            flags.push("The regime is high volatility.".to_owned());
        }
        flags
    }
}

#[async_trait]
impl Advisor for ChecklistAdvisor {
    fn name(&self) -> &str {
        Self::NAME
    }

    async fn advise(&self, input: &AdvisorInput) -> Result<AdviceDraft, AdvisorError> {
        if input.outcome != "enter" {
            return Ok(AdviceDraft {
                stance: AiStance::Abstain,
                confidence: Decimal::ZERO,
                summary: "No trade was proposed; nothing to check.".to_owned(),
                flags: Vec::new(),
            });
        }
        let flags = self.flags(input);
        let (stance, summary) = match flags.len() {
            0 => (
                AiStance::Agree,
                "Every checklist item is comfortable.".to_owned(),
            ),
            1 | 2 => (
                AiStance::Caution,
                format!("{} checklist item(s) need a look.", flags.len()),
            ),
            n => (
                AiStance::Disagree,
                format!(
                    "{n} checklist items are uncomfortable; the checklist would pass on this one."
                ),
            ),
        };
        Ok(AdviceDraft {
            stance,
            confidence: Decimal::new(6, 1),
            summary,
            flags,
        })
    }
}
