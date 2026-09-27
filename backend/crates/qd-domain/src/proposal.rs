//! Trade proposals (spec §6.4).
//!
//! A proposal is complete or it is not a proposal. [`TradeProposal::build`]
//! rounds the plan, computes the economics and expected value itself, and
//! refuses anything incomplete, so a proposal's numbers always agree with its
//! plan. Proposals carry per-unit economics only; the Risk Gate decides the quantity.

use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::economics::{
    CostEstimate, ExpectedValue, OutcomeProbabilities, SlippageAssumption, UnitEconomics,
};
use crate::ids::{AiReviewId, InstrumentId, ProposalId, SnapshotId, StrategyId, StrategyVersionId};
use crate::instrument::InstrumentSpec;
use crate::plan::{PlanDefect, TradePlan, TradePlanInput};

/// Which kind of account the proposal is for. Paper is the default everywhere (INV-14).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountMode {
    /// Historical simulation.
    Backtest,
    /// Paper trading on live data.
    #[default]
    Paper,
    /// Live trading.
    Live,
}

/// Setup quality grade assigned by the strategy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Grade {
    /// Best.
    A,
    /// Good.
    B,
    /// Marginal.
    C,
}

/// The exact strategy version that produced the proposal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StrategyRef {
    /// Strategy family.
    pub strategy_id: StrategyId,
    /// Strategy name.
    pub name: String,
    /// Immutable version.
    pub version_id: StrategyVersionId,
    /// Version number.
    pub version_number: u32,
    /// Logic version of the strategy code.
    pub logic_version: String,
    /// Git commit of the code that produced the proposal.
    pub git_sha: String,
}

/// Whether a reason supports or opposes the trade.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasonDirection {
    /// Argues for the trade.
    Supports,
    /// Argues against the trade.
    Opposes,
}

/// A factor value, numeric or categorical.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FactorValue {
    /// A number, for example a z-score.
    Number(Decimal),
    /// A category, for example a regime name.
    Text(String),
}

/// One structured reason.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reason {
    /// The factor.
    pub factor: String,
    /// Its value.
    pub value: FactorValue,
    /// For or against.
    pub direction: ReasonDirection,
}

/// Why the strategy proposes the trade.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Explanation {
    /// Structured reasons. At least one.
    pub reasons: Vec<Reason>,
    /// The strongest argument against the trade. Required.
    pub strongest_argument_against: String,
    /// Optional AI review. Advisory only (INV-04).
    pub ai_review: Option<AiReviewId>,
}

/// What is needed to replay the proposal to the identical result (INV-09).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reproducibility {
    /// Immutable data snapshot.
    pub snapshot_id: SnapshotId,
    /// Feature-set version.
    pub feature_set_version: String,
    /// Trading-calendar version.
    pub calendar_version: String,
}

/// Everything needed to build a proposal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProposalDraft {
    /// Proposal id.
    pub id: ProposalId,
    /// When the proposal was built.
    pub created_at: DateTime<Utc>,
    /// Decision timestamp: the proposal uses only data knowable at this instant.
    pub as_of: DateTime<Utc>,
    /// The venue-calendar trading date of `as_of`.
    pub trading_date: NaiveDate,
    /// Account mode.
    pub account_mode: AccountMode,
    /// Setup type, for example `pullback_in_uptrend`.
    pub setup_type: String,
    /// Grade.
    pub grade: Grade,
    /// Producing strategy version.
    pub strategy: StrategyRef,
    /// Raw plan levels.
    pub plan: TradePlanInput,
    /// Round-trip costs.
    pub costs: CostEstimate,
    /// Slippage assumption.
    pub slippage: SlippageAssumption,
    /// Outcome probabilities for this exact plan.
    pub probabilities: OutcomeProbabilities,
    /// `E[net P&L per unit | time exit]` in the instrument currency.
    pub time_exit_pnl_per_unit: Decimal,
    /// Explanation.
    pub explanation: Explanation,
    /// Reproducibility references.
    pub reproducibility: Reproducibility,
}

/// Why a proposal could not be built.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProposalError {
    /// A required field is empty.
    #[error("proposal is incomplete: {0} is empty")]
    Incomplete(&'static str),
    /// `as_of` is after `created_at`.
    #[error("decision timestamp is after the creation time")]
    AsOfAfterCreation,
    /// The instrument spec does not apply on the trading date.
    #[error("instrument spec version is not effective on the trading date")]
    InstrumentNotEffective,
    /// The plan or its economics are invalid.
    #[error(transparent)]
    Plan(#[from] PlanDefect),
}

/// The instrument a proposal is for, pinned to a spec version.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstrumentRef {
    /// Instrument id.
    pub id: InstrumentId,
    /// Symbol.
    pub symbol: String,
    /// Spec version used.
    pub spec_version: u32,
}

/// A complete, validated trade proposal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TradeProposal {
    id: ProposalId,
    created_at: DateTime<Utc>,
    as_of: DateTime<Utc>,
    trading_date: NaiveDate,
    account_mode: AccountMode,
    instrument: InstrumentRef,
    setup_type: String,
    grade: Grade,
    strategy: StrategyRef,
    plan: TradePlan,
    economics: UnitEconomics,
    costs: CostEstimate,
    slippage: SlippageAssumption,
    probabilities: OutcomeProbabilities,
    time_exit_pnl_per_unit: Decimal,
    expected_value: ExpectedValue,
    explanation: Explanation,
    reproducibility: Reproducibility,
}

fn required(value: &str, field: &'static str) -> Result<(), ProposalError> {
    if value.trim().is_empty() {
        Err(ProposalError::Incomplete(field))
    } else {
        Ok(())
    }
}

impl TradeProposal {
    /// Builds a proposal: validates completeness, rounds the plan to ticks,
    /// computes the per-unit economics and the expected value.
    pub fn build(draft: ProposalDraft, spec: &InstrumentSpec) -> Result<Self, ProposalError> {
        required(&draft.setup_type, "setup_type")?;
        required(&draft.strategy.name, "strategy.name")?;
        required(&draft.strategy.logic_version, "strategy.logic_version")?;
        required(&draft.strategy.git_sha, "strategy.git_sha")?;
        required(
            &draft.explanation.strongest_argument_against,
            "explanation.strongest_argument_against",
        )?;
        if draft.explanation.reasons.is_empty() {
            return Err(ProposalError::Incomplete("explanation.reasons"));
        }
        required(
            &draft.reproducibility.feature_set_version,
            "reproducibility.feature_set_version",
        )?;
        required(
            &draft.reproducibility.calendar_version,
            "reproducibility.calendar_version",
        )?;
        if draft.as_of > draft.created_at {
            return Err(ProposalError::AsOfAfterCreation);
        }
        if !spec.is_effective_on(draft.trading_date) {
            return Err(ProposalError::InstrumentNotEffective);
        }

        let plan = TradePlan::new(draft.plan, spec)?;
        let economics = UnitEconomics::compute(&plan, spec, &draft.costs, &draft.slippage)?;
        let expected_value = ExpectedValue::compute(
            &economics,
            &draft.probabilities,
            draft.time_exit_pnl_per_unit,
        )?;

        Ok(Self {
            id: draft.id,
            created_at: draft.created_at,
            as_of: draft.as_of,
            trading_date: draft.trading_date,
            account_mode: draft.account_mode,
            instrument: InstrumentRef {
                id: spec.id,
                symbol: spec.symbol.clone(),
                spec_version: spec.version,
            },
            setup_type: draft.setup_type,
            grade: draft.grade,
            strategy: draft.strategy,
            plan,
            economics,
            costs: draft.costs,
            slippage: draft.slippage,
            probabilities: draft.probabilities,
            time_exit_pnl_per_unit: draft.time_exit_pnl_per_unit,
            expected_value,
            explanation: draft.explanation,
            reproducibility: draft.reproducibility,
        })
    }

    /// Proposal id.
    #[must_use]
    pub const fn id(&self) -> ProposalId {
        self.id
    }

    /// When the proposal was built.
    #[must_use]
    pub const fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }

    /// Decision timestamp.
    #[must_use]
    pub const fn as_of(&self) -> DateTime<Utc> {
        self.as_of
    }

    /// Venue-calendar trading date.
    #[must_use]
    pub const fn trading_date(&self) -> NaiveDate {
        self.trading_date
    }

    /// Account mode.
    #[must_use]
    pub const fn account_mode(&self) -> AccountMode {
        self.account_mode
    }

    /// Instrument and spec version.
    #[must_use]
    pub const fn instrument(&self) -> &InstrumentRef {
        &self.instrument
    }

    /// Setup type.
    #[must_use]
    pub fn setup_type(&self) -> &str {
        &self.setup_type
    }

    /// Grade.
    #[must_use]
    pub const fn grade(&self) -> Grade {
        self.grade
    }

    /// Producing strategy version.
    #[must_use]
    pub const fn strategy(&self) -> &StrategyRef {
        &self.strategy
    }

    /// Tick-rounded plan.
    #[must_use]
    pub const fn plan(&self) -> &TradePlan {
        &self.plan
    }

    /// Per-unit economics.
    #[must_use]
    pub const fn economics(&self) -> &UnitEconomics {
        &self.economics
    }

    /// Round-trip costs with line items.
    #[must_use]
    pub const fn costs(&self) -> &CostEstimate {
        &self.costs
    }

    /// Slippage assumption.
    #[must_use]
    pub const fn slippage(&self) -> &SlippageAssumption {
        &self.slippage
    }

    /// Outcome probabilities.
    #[must_use]
    pub const fn probabilities(&self) -> &OutcomeProbabilities {
        &self.probabilities
    }

    /// `E[net P&L per unit | time exit]`.
    #[must_use]
    pub const fn time_exit_pnl_per_unit(&self) -> Decimal {
        self.time_exit_pnl_per_unit
    }

    /// Expected value per unit and in R.
    #[must_use]
    pub const fn expected_value(&self) -> ExpectedValue {
        self.expected_value
    }

    /// Explanation.
    #[must_use]
    pub const fn explanation(&self) -> &Explanation {
        &self.explanation
    }

    /// Reproducibility references.
    #[must_use]
    pub const fn reproducibility(&self) -> &Reproducibility {
        &self.reproducibility
    }
}
