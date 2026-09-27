//! Strategy-version lifecycle (spec §6.6, INV-10, INV-11).
//!
//! Draft → Research → (Rejected | ResearchPassed) → Paper → SmallCapital → Full.
//! Any stage → Retired. Paper, SmallCapital or Full → Suspended (manual) → back
//! to the previous stage. Automatic demotion moves SmallCapital or Full to Paper.
//!
//! Promotion needs an [`OwnerApproval`] that references recorded evidence and
//! goes exactly one stage up. Demotion needs no approval.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::TransitionError;
use crate::ids::{EvidenceId, UserId};

/// A stage in which a version may trade.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TradingStage {
    /// Paper trading only.
    Paper,
    /// Live with a small capital cap.
    SmallCapital,
    /// Live at full allocation.
    Full,
}

impl TradingStage {
    /// The stage one step up, if any.
    #[must_use]
    pub const fn next(self) -> Option<Self> {
        match self {
            Self::Paper => Some(Self::SmallCapital),
            Self::SmallCapital => Some(Self::Full),
            Self::Full => None,
        }
    }
}

/// Lifecycle stage of one strategy version.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "stage", rename_all = "snake_case")]
pub enum StrategyStage {
    /// Being written.
    Draft,
    /// Under offline validation.
    Research,
    /// Failed validation. Terminal apart from retirement.
    Rejected,
    /// Passed offline validation; awaiting promotion to paper.
    ResearchPassed,
    /// Paper trading.
    Paper,
    /// Live, small capital.
    SmallCapital,
    /// Live, full allocation.
    Full,
    /// Manually suspended; resumes to `resume_to`.
    Suspended {
        /// The stage it was suspended from.
        resume_to: TradingStage,
    },
    /// Permanently retired.
    Retired,
}

impl StrategyStage {
    /// The trading stage, if this stage trades.
    #[must_use]
    pub const fn trading_stage(self) -> Option<TradingStage> {
        match self {
            Self::Paper => Some(TradingStage::Paper),
            Self::SmallCapital => Some(TradingStage::SmallCapital),
            Self::Full => Some(TradingStage::Full),
            _ => None,
        }
    }

    /// Whether the version may trade at all (paper or live).
    #[must_use]
    pub const fn can_trade(self) -> bool {
        self.trading_stage().is_some()
    }

    /// Whether the version is eligible for live orders (INV-14).
    #[must_use]
    pub const fn is_live_eligible(self) -> bool {
        matches!(self, Self::SmallCapital | Self::Full)
    }
}

impl From<TradingStage> for StrategyStage {
    fn from(stage: TradingStage) -> Self {
        match stage {
            TradingStage::Paper => Self::Paper,
            TradingStage::SmallCapital => Self::SmallCapital,
            TradingStage::Full => Self::Full,
        }
    }
}

/// An authenticated owner approval backed by recorded evidence (INV-11).
///
/// The application layer creates this only after authenticating the owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerApproval {
    /// The approving owner.
    pub approved_by: UserId,
    /// When they approved.
    pub approved_at: DateTime<Utc>,
    /// The recorded evidence the approval rests on.
    pub evidence: EvidenceId,
}

/// A lifecycle event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum StageEvent {
    /// Draft → Research.
    StartResearch,
    /// Research → Rejected.
    RejectResearch {
        /// Why validation failed.
        reason: String,
    },
    /// Research → ResearchPassed.
    PassResearch {
        /// The validation evidence.
        evidence: EvidenceId,
    },
    /// One stage up: ResearchPassed → Paper → SmallCapital → Full.
    Promote {
        /// Target stage; must be exactly one step up.
        to: TradingStage,
        /// The owner's approval.
        approval: OwnerApproval,
    },
    /// Paper, SmallCapital or Full → Suspended.
    Suspend {
        /// Who suspended it.
        by: UserId,
        /// Why.
        reason: String,
    },
    /// Suspended → the stage it was suspended from.
    Resume {
        /// Who resumed it.
        by: UserId,
    },
    /// SmallCapital or Full → Paper, automatically, on a breach.
    AutoDemote {
        /// The breach.
        reason: String,
    },
    /// Any stage → Retired.
    Retire {
        /// Why.
        reason: String,
    },
}

impl StageEvent {
    /// Event name for errors and logs.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::StartResearch => "start_research",
            Self::RejectResearch { .. } => "reject_research",
            Self::PassResearch { .. } => "pass_research",
            Self::Promote { .. } => "promote",
            Self::Suspend { .. } => "suspend",
            Self::Resume { .. } => "resume",
            Self::AutoDemote { .. } => "auto_demote",
            Self::Retire { .. } => "retire",
        }
    }
}

impl StrategyStage {
    /// Applies an event, returning the next stage or an error for an illegal transition.
    pub fn apply(self, event: &StageEvent) -> Result<Self, TransitionError> {
        use StageEvent as E;
        use StrategyStage as S;
        let next = match (self, event) {
            (S::Retired, _) => None,
            (_, E::Retire { .. }) => Some(S::Retired),
            (S::Draft, E::StartResearch) => Some(S::Research),
            (S::Research, E::RejectResearch { .. }) => Some(S::Rejected),
            (S::Research, E::PassResearch { .. }) => Some(S::ResearchPassed),
            (S::ResearchPassed, E::Promote { to, .. }) => {
                (*to == TradingStage::Paper).then_some(S::Paper)
            }
            (current, E::Promote { to, .. }) => current
                .trading_stage()
                .and_then(TradingStage::next)
                .filter(|next| next == to)
                .map(S::from),
            (current, E::Suspend { .. }) => current
                .trading_stage()
                .map(|resume_to| S::Suspended { resume_to }),
            (S::Suspended { resume_to }, E::Resume { .. }) => Some(S::from(resume_to)),
            (S::SmallCapital | S::Full, E::AutoDemote { .. }) => Some(S::Paper),
            _ => None,
        };
        next.ok_or_else(|| TransitionError::new("strategy stage", self, event.name()))
    }
}
