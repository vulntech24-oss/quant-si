//! Trade Proposal Engine and Decision Engine (spec §5.2, INV-03, INV-05, INV-17).
//!
//! Deterministic gates, in order; the first failure is the NO TRADE reason:
//!
//! 1. Stale data: the series must end on the expected last completed date.
//! 2. Regime: the strategy must be active.
//! 3. No setup: not a decision; the scanner records it compactly.
//! 4. Already in position on the instrument.
//! 5. Evidence: probabilities from validation with at least `min_evidence`
//!    comparable setups (research runs use a neutral prior instead; ADR 0006).
//! 6. A complete proposal (costs quoted at a reference quantity).
//! 7. The Risk Gate, which decides the quantity or rejects.
//!
//! The outcome is always post-risk. An authorization to enter is released
//! only after the decision is durably journaled.
//!
//! The AI agent (ADR 0016) goes through the same gates with its own plan,
//! its own outcome probabilities (in place of step 5) and its capital
//! allocation, which the Risk Gate treats as a maximum: the final quantity
//! is the smaller of what the agent asks for and what the gate allows.

use chrono::{DateTime, NaiveDate, Utc};
use qd_domain::costs::{CostModel, CostRequest};
use qd_domain::economics::{OutcomeProbabilities, SlippageAssumption};
use qd_domain::ids::{DecisionId, ProposalId, SnapshotId};
use qd_domain::instrument::{InstrumentSpec, ProductType};
use qd_domain::lifecycle::strategy::StrategyStage;
use qd_domain::market::BarSeries;
use qd_domain::num::{FxRate, Price, Quantity};
use qd_domain::outcome::{DecisionOutcome, InputKind, NoTradeReason};
use qd_domain::proposal::{
    AccountMode, Explanation, ProposalDraft, ProposalError, Reproducibility, StrategyRef,
    TradeProposal,
};
use qd_risk::config::RiskConfig;
use qd_risk::gate::{AccountRiskState, EntryRequest, RiskGate, RiskVerdict};
use qd_strategy::strategy::{Evaluation, SetupCandidate, StrategyOutput};
use rust_decimal::Decimal;

use crate::journal::{DecisionRecord, JournalEntry};
use crate::orders::EntryAuthorization;
use crate::ports::{EvidenceSource, Journal, JournalError};

/// Where outcome probabilities come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvidencePolicy {
    /// Paper and live: validated evidence tables, at least `min_evidence` setups.
    Required {
        /// Minimum comparable out-of-sample setups.
        min_evidence: u32,
    },
    /// Offline research backtests that produce the evidence: a neutral prior
    /// (all mass on the time exit, EV 0). Never valid for paper or live accounts.
    ResearchPrior,
}

/// What the AI agent brings to an entry decision (ADR 0016).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentForecast {
    /// The agent's outcome probabilities (source `ai:<provider>:<model>`).
    pub probabilities: OutcomeProbabilities,
    /// Expected R when neither stop nor target is hit before the time exit.
    pub time_exit_r: Decimal,
    /// The allocation as a quantity: the most the agent asks to trade.
    pub max_quantity: Quantity,
}

/// One strategy version as the Decision Engine sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StrategyVersionInfo {
    /// Identity.
    pub reference: StrategyRef,
    /// Current stage.
    pub stage: StrategyStage,
    /// Net RR floor of the version.
    pub rr_floor: Decimal,
    /// Slippage assumption for its plans.
    pub slippage: SlippageAssumption,
}

/// Everything one decision needs.
#[derive(Clone, Copy)]
pub struct DecisionContext<'a> {
    /// Decision id.
    pub decision_id: DecisionId,
    /// Decision time.
    pub at: DateTime<Utc>,
    /// Last completed trading date per the venue calendar.
    pub expected_last_completed: NaiveDate,
    /// Instrument.
    pub spec: &'a InstrumentSpec,
    /// Product to trade.
    pub product: ProductType,
    /// Instrument → account currency rate.
    pub fx: FxRate,
    /// The strategy version.
    pub strategy: &'a StrategyVersionInfo,
    /// The strategy evaluation on `series`.
    pub evaluation: &'a Evaluation,
    /// Completed bars the evaluation used.
    pub series: &'a BarSeries,
    /// Account state for the Risk Gate.
    pub account: &'a AccountRiskState,
    /// Whether a position in this instrument is open or opening.
    pub already_in_position: bool,
    /// Immutable data snapshot the evaluation used.
    pub snapshot_id: SnapshotId,
    /// Trading-calendar version.
    pub calendar_version: &'a str,
}

/// What the Decision Engine concluded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecisionResult {
    /// Active strategy, no setup: not a decision.
    NoSetup,
    /// A decision (Enter or NoTrade).
    Decided(Box<DecisionRecord>),
}

/// A journaled decision and, for approved entries, the authorization to trade.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JournaledDecision {
    /// The record as journaled.
    pub record: DecisionRecord,
    /// Present only for an approved entry.
    pub authorization: Option<EntryAuthorization>,
}

/// The Decision Engine.
#[derive(Clone, Copy)]
pub struct DecisionEngine<'a> {
    risk: &'a RiskConfig,
    costs: &'a dyn CostModel,
    evidence: &'a dyn EvidenceSource,
    policy: EvidencePolicy,
}

impl std::fmt::Debug for DecisionEngine<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecisionEngine")
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

impl<'a> DecisionEngine<'a> {
    /// Creates the engine.
    #[must_use]
    pub const fn new(
        risk: &'a RiskConfig,
        costs: &'a dyn CostModel,
        evidence: &'a dyn EvidenceSource,
        policy: EvidencePolicy,
    ) -> Self {
        Self {
            risk,
            costs,
            evidence,
            policy,
        }
    }

    /// Decides, without journaling. Pure given its inputs.
    #[must_use]
    pub fn decide(&self, ctx: &DecisionContext<'_>) -> DecisionResult {
        self.decide_with(ctx, None)
    }

    /// Decides an AI agent's entry, without journaling: the same gates, with
    /// the agent's probabilities and its allocation as the size ceiling.
    #[must_use]
    pub fn decide_agent(
        &self,
        ctx: &DecisionContext<'_>,
        forecast: &AgentForecast,
    ) -> DecisionResult {
        self.decide_with(ctx, Some(forecast))
    }

    fn decide_with(
        &self,
        ctx: &DecisionContext<'_>,
        forecast: Option<&AgentForecast>,
    ) -> DecisionResult {
        let record = |outcome: DecisionOutcome,
                      proposal: Option<TradeProposal>,
                      approval: Option<qd_risk::gate::ApprovedEntry>| {
            DecisionResult::Decided(Box::new(DecisionRecord {
                id: ctx.decision_id,
                at: ctx.at,
                as_of_date: ctx.series.last_completed(),
                account: ctx.account.account,
                instrument: ctx.spec.id,
                strategy: ctx.strategy.reference.clone(),
                regime: ctx.evaluation.regime,
                feature_set_version: ctx.evaluation.features.version.to_owned(),
                outcome,
                proposal,
                approval,
            }))
        };
        let no_trade =
            |reason: NoTradeReason| record(DecisionOutcome::NoTrade { reason }, None, None);

        // Research priors are never valid outside backtests.
        if forecast.is_none()
            && self.policy == EvidencePolicy::ResearchPrior
            && ctx.account.mode != AccountMode::Backtest
        {
            return no_trade(NoTradeReason::MissingOrInconsistentData {
                input: InputKind::Configuration,
            });
        }
        let last_bar = ctx.series.last().map(qd_domain::market::Bar::date);
        if last_bar != Some(ctx.expected_last_completed) {
            return no_trade(NoTradeReason::StaleData {
                input: InputKind::Bars,
            });
        }
        let candidate = match &ctx.evaluation.output {
            StrategyOutput::Inactive { regime } => {
                return no_trade(NoTradeReason::StrategyInactiveInRegime {
                    regime: regime.name().to_owned(),
                });
            }
            StrategyOutput::NoSetup => return DecisionResult::NoSetup,
            StrategyOutput::Setup(candidate) => candidate,
        };
        if ctx.already_in_position {
            return no_trade(NoTradeReason::AlreadyInPosition);
        }
        let probabilities = match forecast {
            Some(f) => Ok((f.probabilities.clone(), f.time_exit_r)),
            None => self.probabilities(ctx, candidate),
        };
        let (probabilities, time_exit_r) = match probabilities {
            Ok(p) => p,
            Err(reason) => return no_trade(reason),
        };
        let proposal = match self.proposal(ctx, candidate, probabilities, time_exit_r) {
            Ok(p) => p,
            Err(reason) => return no_trade(reason),
        };
        let gate = RiskGate::new(self.risk, self.costs);
        let verdict = gate.evaluate(
            &EntryRequest {
                proposal: &proposal,
                spec: ctx.spec,
                product: ctx.product,
                stage: ctx.strategy.stage,
                rr_floor: ctx.strategy.rr_floor,
                fx: ctx.fx,
                at: ctx.at,
                max_quantity: forecast.map(|f| f.max_quantity),
            },
            ctx.account,
        );
        match verdict {
            RiskVerdict::Approved(approved) => record(
                DecisionOutcome::Enter {
                    action: proposal.plan().action(),
                },
                Some(proposal),
                Some(*approved),
            ),
            RiskVerdict::Rejected(reason) => {
                record(DecisionOutcome::NoTrade { reason }, Some(proposal), None)
            }
        }
    }

    /// Decides and journals. The entry authorization exists only after the
    /// journal write succeeded (INV-05).
    pub async fn decide_and_journal(
        &self,
        ctx: &DecisionContext<'_>,
        journal: &dyn Journal,
    ) -> Result<Option<JournaledDecision>, JournalError> {
        Self::journal_decision(self.decide(ctx), journal).await
    }

    /// Decides an AI agent's entry and journals it; the authorization exists
    /// only after the journal write succeeded (INV-05).
    pub async fn decide_agent_and_journal(
        &self,
        ctx: &DecisionContext<'_>,
        forecast: &AgentForecast,
        journal: &dyn Journal,
    ) -> Result<Option<JournaledDecision>, JournalError> {
        Self::journal_decision(self.decide_agent(ctx, forecast), journal).await
    }

    async fn journal_decision(
        result: DecisionResult,
        journal: &dyn Journal,
    ) -> Result<Option<JournaledDecision>, JournalError> {
        let DecisionResult::Decided(record) = result else {
            return Ok(None);
        };
        journal
            .append(&JournalEntry::Decision(record.clone()))
            .await?;
        let authorization = match (&record.outcome, &record.approval) {
            (DecisionOutcome::Enter { action }, Some(approval)) => Some(EntryAuthorization::new(
                record.id,
                record.account,
                record.instrument,
                Some(record.strategy.version_id),
                *action,
                approval.quantity,
            )),
            _ => None,
        };
        Ok(Some(JournaledDecision {
            record: *record,
            authorization,
        }))
    }

    fn probabilities(
        &self,
        ctx: &DecisionContext<'_>,
        candidate: &SetupCandidate,
    ) -> Result<(OutcomeProbabilities, Decimal), NoTradeReason> {
        match self.policy {
            EvidencePolicy::ResearchPrior => OutcomeProbabilities::new(
                Decimal::ZERO,
                Decimal::ZERO,
                Decimal::ONE,
                "research-prior",
                0,
            )
            .map(|p| (p, Decimal::ZERO))
            .map_err(|defect| NoTradeReason::InvalidTradePlan { defect }),
            EvidencePolicy::Required { min_evidence } => {
                let Some(evidence) = self
                    .evidence
                    .evidence(ctx.strategy.reference.version_id, &candidate.setup_type)
                else {
                    return Err(NoTradeReason::InsufficientEvidence {
                        n: 0,
                        min: min_evidence,
                    });
                };
                let n = evidence.probabilities.evidence_count();
                if n < min_evidence {
                    return Err(NoTradeReason::InsufficientEvidence {
                        n,
                        min: min_evidence,
                    });
                }
                Ok((evidence.probabilities, evidence.time_exit_r))
            }
        }
    }

    /// The quantity costs are quoted at: roughly what the Risk Gate will size,
    /// never below the minimum order quantity.
    fn reference_quantity(
        &self,
        ctx: &DecisionContext<'_>,
        candidate: &SetupCandidate,
    ) -> Option<Quantity> {
        let risk_points = (candidate.plan.entry - candidate.plan.stop).abs();
        let per_unit = risk_points
            .checked_mul(ctx.spec.multiplier)?
            .checked_mul(ctx.fx.rate())?;
        let budget = ctx
            .account
            .equity
            .amount
            .checked_mul(self.risk.risk_per_trade.value())?;
        let raw = budget.checked_div(per_unit)?;
        let rounded = ctx.spec.round_quantity_down(raw).ok()?;
        Some(rounded.max(Quantity::new(ctx.spec.min_quantity).ok()?))
    }

    fn proposal(
        &self,
        ctx: &DecisionContext<'_>,
        candidate: &SetupCandidate,
        probabilities: OutcomeProbabilities,
        time_exit_r: Decimal,
    ) -> Result<TradeProposal, NoTradeReason> {
        let config = || NoTradeReason::MissingOrInconsistentData {
            input: InputKind::Configuration,
        };
        let plan = &candidate.plan;
        let quantity = self.reference_quantity(ctx, candidate).ok_or_else(config)?;
        let (entry, exit) = match (
            Price::new(plan.entry),
            Price::new(plan.stop.max(plan.target)),
        ) {
            (Ok(e), Ok(x)) => (e, x),
            _ => {
                return Err(NoTradeReason::InvalidTradePlan {
                    defect: qd_domain::plan::PlanDefect::NonPositiveLevel {
                        level: qd_domain::plan::PlanLevel::Entry,
                    },
                });
            }
        };
        let quote = self
            .costs
            .quote(&CostRequest {
                spec: ctx.spec,
                product: ctx.product,
                side: plan.action.side(),
                quantity,
                entry,
                exit,
                trade_date: ctx.series.last_completed(),
            })
            .map_err(|_| config())?;
        // E[P&L | time exit] per unit, from R using the gross risk per unit.
        let risk_gross = (plan.entry - plan.stop).abs() * ctx.spec.multiplier;
        let time_exit_pnl_per_unit = time_exit_r * risk_gross;
        let draft = ProposalDraft {
            id: ProposalId::from_uuid(*ctx.decision_id.as_uuid()),
            created_at: ctx.at,
            as_of: ctx.at,
            trading_date: ctx.series.last_completed(),
            account_mode: ctx.account.mode,
            setup_type: candidate.setup_type.clone(),
            grade: candidate.grade,
            strategy: ctx.strategy.reference.clone(),
            plan: candidate.plan.clone(),
            costs: quote.estimate,
            slippage: ctx.strategy.slippage.clone(),
            probabilities,
            time_exit_pnl_per_unit,
            explanation: Explanation {
                reasons: candidate.reasons.clone(),
                strongest_argument_against: candidate.strongest_argument_against.clone(),
                ai_review: None,
            },
            reproducibility: Reproducibility {
                snapshot_id: ctx.snapshot_id,
                feature_set_version: ctx.evaluation.features.version.to_owned(),
                calendar_version: ctx.calendar_version.to_owned(),
            },
        };
        TradeProposal::build(draft, ctx.spec).map_err(|error| match error {
            ProposalError::Plan(defect) => NoTradeReason::InvalidTradePlan { defect },
            ProposalError::InstrumentNotEffective => NoTradeReason::MissingOrInconsistentData {
                input: InputKind::InstrumentSpec,
            },
            ProposalError::Incomplete(_) | ProposalError::AsOfAfterCreation => config(),
        })
    }
}
