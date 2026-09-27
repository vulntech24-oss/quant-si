//! Entry evaluation: the Risk Gate (spec §6.5 "Sizing", INV-02, INV-03, INV-06, INV-14).
//!
//! Checks run in this order, and the first failure decides the NO TRADE reason:
//!
//! 1. Halts: any active halt covering the account, strategy version or
//!    instrument, or an unknown halt state, blocks the entry.
//! 2. Input consistency: spec version, account mode, timestamps, currencies,
//!    equity and FX must be present and agree (fail closed).
//! 3. Stage and direction: the version must be at a trading stage (live
//!    accounts need SmallCapital or Full); shorts need an instrument that allows
//!    overnight shorts.
//! 4. Proposal gates: net RR at or above the strategy's floor, EV at or above
//!    the configured minimum.
//! 5. Loss limits: daily, weekly, drawdown and the consecutive-loss cool-off.
//! 6. Sizing (steps 1–2), then caps on total, bucket and strategy open risk
//!    (step 3), then costs recomputed at the final quantity with RR and EV
//!    re-checked (step 4), repeated until the size is stable. Below the minimum
//!    quantity the entry is too small (step 5).

use chrono::{DateTime, Utc};
use qd_domain::action::RiskEffect;
use qd_domain::costs::{CostModel, CostRequest};
use qd_domain::economics::{CostEstimate, ExpectedValue, UnitEconomics};
use qd_domain::halt::{HaltState, OrderContext, check_order};
use qd_domain::ids::{AccountId, InstrumentId, StrategyVersionId};
use qd_domain::instrument::{CorrelationBucket, InstrumentSpec, ProductType};
use qd_domain::lifecycle::strategy::StrategyStage;
use qd_domain::num::{FxRate, Money, Quantity, Ratio};
use qd_domain::outcome::{InputKind, NoTradeReason, RiskLimitBreach};
use qd_domain::plan::PlanDefect;
use qd_domain::portfolio::drawdown;
use qd_domain::proposal::{AccountMode, TradeProposal};
use qd_domain::sizing::{SizingInput, SizingOutcome, cap_quantity, size_position};
use rust_decimal::Decimal;
use serde::Serialize;

use crate::config::RiskConfig;

/// Sizing and cost recomputation stop after this many rounds without a stable size.
const MAX_SIZING_ROUNDS: usize = 8;

/// Open risk already committed on the account: one position or working entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RiskItem {
    /// Instrument.
    pub instrument: InstrumentId,
    /// Strategy version that owns it; `None` for manual trades.
    pub strategy_version: Option<StrategyVersionId>,
    /// Correlated-risk bucket.
    pub bucket: CorrelationBucket,
    /// Open risk in the account currency (see `qd_domain::portfolio`).
    pub amount: Money,
}

/// Account state the Risk Gate needs. All money is in the account currency.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountRiskState {
    /// Account.
    pub account: AccountId,
    /// Backtest, paper or live.
    pub mode: AccountMode,
    /// Current equity (cash + mark-to-market).
    pub equity: Money,
    /// Equity at the start of the trading day.
    pub equity_at_day_start: Money,
    /// Equity at the start of the week.
    pub equity_at_week_start: Money,
    /// Equity high-water mark.
    pub high_water_mark: Money,
    /// Losing trades in a row.
    pub consecutive_losses: u32,
    /// Open risk of every position and working entry.
    pub open_risk: Vec<RiskItem>,
    /// Kill-switch state.
    pub halts: HaltState,
}

/// One entry to evaluate.
#[derive(Clone, Copy, Debug)]
pub struct EntryRequest<'a> {
    /// The complete proposal.
    pub proposal: &'a TradeProposal,
    /// The instrument spec version the proposal was built with.
    pub spec: &'a InstrumentSpec,
    /// Product to trade.
    pub product: ProductType,
    /// Current stage of the proposing strategy version.
    pub stage: StrategyStage,
    /// The strategy version's net RR floor.
    pub rr_floor: Decimal,
    /// Point-in-time rate from the instrument currency into the account currency.
    pub fx: FxRate,
    /// Decision time.
    pub at: DateTime<Utc>,
}

/// An approved entry: the only place a quantity is decided.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ApprovedEntry {
    /// Quantity to trade, a valid order size.
    pub quantity: Quantity,
    /// Stage multiplier applied.
    pub stage_multiplier: Decimal,
    /// `equity × risk_per_trade × stage_multiplier`.
    pub risk_budget: Money,
    /// `quantity × risk_net` at the final costs, in the account currency.
    pub planned_risk: Money,
    /// Costs at the final quantity.
    pub costs: CostEstimate,
    /// Whether those costs come from a verified schedule.
    pub costs_verified: bool,
    /// Per-unit economics at the final costs.
    pub economics: UnitEconomics,
    /// Expected value at the final costs.
    pub expected_value: ExpectedValue,
}

/// The Risk Gate's decision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum RiskVerdict {
    /// Take the entry at this size.
    Approved(Box<ApprovedEntry>),
    /// Do not trade, for this reason.
    Rejected(NoTradeReason),
}

type Check<T> = Result<T, NoTradeReason>;

fn inconsistent(input: InputKind) -> NoTradeReason {
    NoTradeReason::MissingOrInconsistentData { input }
}

fn overflow() -> NoTradeReason {
    NoTradeReason::InvalidTradePlan {
        defect: PlanDefect::ArithmeticOverflow,
    }
}

fn arith(value: Option<Decimal>) -> Check<Decimal> {
    value.ok_or_else(overflow)
}

fn ratio(numerator: Decimal, denominator: Decimal) -> Check<Ratio> {
    Ratio::new(arith(numerator.checked_div(denominator))?).map_err(|_| overflow())
}

/// Loss since `start` as a fraction of `start`; zero when equity is up.
fn loss_fraction(start: Money, now: Money) -> Check<Ratio> {
    if now.amount >= start.amount {
        return Ok(Ratio::ZERO);
    }
    ratio(arith(start.amount.checked_sub(now.amount))?, start.amount)
}

/// Which open-risk cap.
#[derive(Clone)]
enum Cap {
    Total,
    Bucket(CorrelationBucket),
    Strategy,
}

impl Cap {
    fn breach(self, limit: Ratio, would_be: Ratio) -> RiskLimitBreach {
        match self {
            Self::Total => RiskLimitBreach::TotalOpenRisk { limit, would_be },
            Self::Bucket(bucket) => RiskLimitBreach::CorrelatedBucket {
                bucket,
                limit,
                would_be,
            },
            Self::Strategy => RiskLimitBreach::StrategyVersion { limit, would_be },
        }
    }
}

/// The Risk Gate.
#[derive(Clone, Copy)]
pub struct RiskGate<'a> {
    config: &'a RiskConfig,
    costs: &'a dyn CostModel,
}

impl std::fmt::Debug for RiskGate<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RiskGate")
            .field("config", self.config)
            .finish_non_exhaustive()
    }
}

impl<'a> RiskGate<'a> {
    /// Creates a gate over a configuration and the shared cost model.
    #[must_use]
    pub const fn new(config: &'a RiskConfig, costs: &'a dyn CostModel) -> Self {
        Self { config, costs }
    }

    /// Evaluates one entry against the account state.
    #[must_use]
    pub fn evaluate(&self, request: &EntryRequest<'_>, state: &AccountRiskState) -> RiskVerdict {
        match self.try_evaluate(request, state) {
            Ok(approved) => RiskVerdict::Approved(Box::new(approved)),
            Err(reason) => RiskVerdict::Rejected(reason),
        }
    }

    fn try_evaluate(
        &self,
        request: &EntryRequest<'_>,
        state: &AccountRiskState,
    ) -> Check<ApprovedEntry> {
        let proposal = request.proposal;
        let context = OrderContext {
            account: state.account,
            strategy_version: Some(proposal.strategy().version_id),
            instrument: proposal.instrument().id,
        };
        check_order(RiskEffect::Increasing, &state.halts, &context, request.at)
            .map_err(NoTradeReason::KillSwitch)?;
        check_inputs(request, state)?;
        let stage_multiplier = self.stage_multiplier(request, state)?;
        let side = proposal.plan().action().side();
        if !request.spec.permits_overnight(side) {
            return Err(NoTradeReason::ShortNotPermitted);
        }
        self.check_proposal_gates(request)?;
        self.check_loss_limits(state)?;
        self.size(request, state, stage_multiplier)
    }

    fn stage_multiplier(
        &self,
        request: &EntryRequest<'_>,
        state: &AccountRiskState,
    ) -> Check<Decimal> {
        let not_eligible = || {
            NoTradeReason::RiskLimit(RiskLimitBreach::StageNotEligible {
                stage: request.stage,
            })
        };
        let trading_stage = request.stage.trading_stage().ok_or_else(not_eligible)?;
        let multiplier = match state.mode {
            AccountMode::Live => {
                if !request.stage.is_live_eligible() {
                    return Err(not_eligible());
                }
                self.config.stage_multipliers.for_stage(trading_stage)
            }
            AccountMode::Paper | AccountMode::Backtest => {
                if self.config.paper_accounts_size_as_full {
                    self.config.stage_multipliers.full
                } else {
                    self.config.stage_multipliers.for_stage(trading_stage)
                }
            }
        };
        if multiplier.is_zero() {
            return Err(not_eligible());
        }
        Ok(multiplier)
    }

    fn check_proposal_gates(&self, request: &EntryRequest<'_>) -> Check<()> {
        let proposal = request.proposal;
        let rr = proposal.economics().rr_net();
        if rr < request.rr_floor {
            return Err(NoTradeReason::RiskRewardBelowFloor {
                rr,
                floor: request.rr_floor,
            });
        }
        let ev_r = proposal.expected_value().in_r();
        if ev_r < self.config.min_ev_r {
            return Err(NoTradeReason::InsufficientEdge {
                ev_r,
                min: self.config.min_ev_r,
            });
        }
        Ok(())
    }

    fn check_loss_limits(&self, state: &AccountRiskState) -> Check<()> {
        let config = self.config;
        let daily = loss_fraction(state.equity_at_day_start, state.equity)?;
        if daily >= config.daily_loss_limit {
            return Err(NoTradeReason::RiskLimit(RiskLimitBreach::DailyLoss {
                limit: config.daily_loss_limit,
                current: daily,
            }));
        }
        let weekly = loss_fraction(state.equity_at_week_start, state.equity)?;
        if weekly >= config.weekly_loss_limit {
            return Err(NoTradeReason::RiskLimit(RiskLimitBreach::WeeklyLoss {
                limit: config.weekly_loss_limit,
                current: weekly,
            }));
        }
        let dd = drawdown(state.high_water_mark, state.equity)
            .map_err(|_| inconsistent(InputKind::Equity))?;
        if dd >= config.hard_halt_drawdown {
            return Err(NoTradeReason::RiskLimit(RiskLimitBreach::Drawdown {
                limit: config.hard_halt_drawdown,
                current: dd,
            }));
        }
        if state.consecutive_losses >= config.cool_off_after_losses {
            return Err(NoTradeReason::RiskLimit(
                RiskLimitBreach::ConsecutiveLosses {
                    losses: state.consecutive_losses,
                    limit: config.cool_off_after_losses,
                },
            ));
        }
        Ok(())
    }

    fn caps(
        &self,
        request: &EntryRequest<'_>,
        state: &AccountRiskState,
    ) -> Check<Vec<(Cap, Ratio, Decimal)>> {
        let version = request.proposal.strategy().version_id;
        let bucket = &request.spec.correlation_bucket;
        let sum = |keep: &dyn Fn(&RiskItem) -> bool| -> Check<Decimal> {
            state
                .open_risk
                .iter()
                .filter(|item| keep(item))
                .try_fold(Decimal::ZERO, |acc, item| {
                    arith(acc.checked_add(item.amount.amount))
                })
        };
        Ok(vec![
            (Cap::Total, self.config.max_total_open_risk, sum(&|_| true)?),
            (
                Cap::Bucket(bucket.clone()),
                self.config.max_bucket_open_risk,
                sum(&|item| &item.bucket == bucket)?,
            ),
            (
                Cap::Strategy,
                self.config.max_strategy_open_risk,
                sum(&|item| item.strategy_version == Some(version))?,
            ),
        ])
    }

    fn size(
        &self,
        request: &EntryRequest<'_>,
        state: &AccountRiskState,
        stage_multiplier: Decimal,
    ) -> Check<ApprovedEntry> {
        let proposal = request.proposal;
        let spec = request.spec;
        let plan = proposal.plan();
        let equity = state.equity;
        let caps = self.caps(request, state)?;
        let mut risk_net_per_unit = proposal.economics().risk_net();

        for _ in 0..MAX_SIZING_ROUNDS {
            let input = SizingInput {
                equity,
                risk_per_trade: self.config.risk_per_trade,
                stage_multiplier,
                risk_net_per_unit: Money::new(risk_net_per_unit, spec.currency),
                fx: request.fx,
            };
            let mut size = match size_position(&input, spec) {
                Ok(SizingOutcome::Sized(size)) => size,
                Ok(SizingOutcome::TooSmall { quantity, min }) => {
                    return Err(NoTradeReason::PositionTooSmall { quantity, min });
                }
                Err(_) => return Err(inconsistent(InputKind::Equity)),
            };

            for (cap, limit, current) in &caps {
                let limit_amount = arith(equity.amount.checked_mul(limit.value()))?;
                let after = arith(current.checked_add(size.planned_risk().amount))?;
                if after <= limit_amount {
                    continue;
                }
                let would_be = ratio(after, equity.amount)?;
                let headroom = arith(limit_amount.checked_sub(*current))?;
                let cap_units = if headroom > Decimal::ZERO {
                    arith(headroom.checked_div(size.risk_per_unit().amount))?
                } else {
                    Decimal::ZERO
                };
                let cap_units = Quantity::new(cap_units).map_err(|_| overflow())?;
                let capped = cap_quantity(&size, cap_units, spec)
                    .map_err(|_| inconsistent(InputKind::Equity))?;
                size = match capped {
                    SizingOutcome::Sized(capped) => capped,
                    SizingOutcome::TooSmall { .. } => {
                        return Err(NoTradeReason::RiskLimit(
                            cap.clone().breach(*limit, would_be),
                        ));
                    }
                };
            }

            // Step 4: costs at the final quantity. The exit leg is priced at the
            // higher plan level, which has the larger turnover.
            let quote = self
                .costs
                .quote(&CostRequest {
                    spec,
                    product: request.product,
                    side: plan.action().side(),
                    quantity: size.quantity(),
                    entry: plan.entry().price,
                    exit: plan.stop().max(plan.target()),
                    trade_date: proposal.trading_date(),
                })
                .map_err(|_| inconsistent(InputKind::Configuration))?;
            if state.mode == AccountMode::Live && !quote.verified {
                return Err(inconsistent(InputKind::Configuration));
            }
            let economics =
                match UnitEconomics::compute(plan, spec, &quote.estimate, proposal.slippage()) {
                    Ok(economics) => economics,
                    Err(PlanDefect::NonPositiveNetReward) => {
                        return Err(NoTradeReason::UneconomicAfterCosts);
                    }
                    Err(defect) => return Err(NoTradeReason::InvalidTradePlan { defect }),
                };
            if economics.risk_net() > risk_net_per_unit {
                // Per-unit costs rose at this smaller size; size again with them.
                risk_net_per_unit = economics.risk_net();
                continue;
            }
            let expected_value = ExpectedValue::compute(
                &economics,
                proposal.probabilities(),
                proposal.time_exit_pnl_per_unit(),
            )
            .map_err(|defect| NoTradeReason::InvalidTradePlan { defect })?;
            if economics.rr_net() < request.rr_floor || expected_value.in_r() < self.config.min_ev_r
            {
                return Err(NoTradeReason::UneconomicAfterCosts);
            }
            let planned = arith(economics.risk_net().checked_mul(size.quantity().value()))?;
            let planned_risk = request
                .fx
                .convert(Money::new(planned, spec.currency))
                .map_err(|_| inconsistent(InputKind::FxRates))?;
            return Ok(ApprovedEntry {
                quantity: size.quantity(),
                stage_multiplier,
                risk_budget: size.risk_budget(),
                planned_risk,
                costs: quote.estimate,
                costs_verified: quote.verified,
                economics,
                expected_value,
            });
        }
        Err(NoTradeReason::UneconomicAfterCosts)
    }
}

/// Fails closed on missing or inconsistent inputs (INV-06).
fn check_inputs(request: &EntryRequest<'_>, state: &AccountRiskState) -> Check<()> {
    let proposal = request.proposal;
    let spec = request.spec;
    if proposal.instrument().id != spec.id || proposal.instrument().spec_version != spec.version {
        return Err(inconsistent(InputKind::InstrumentSpec));
    }
    if proposal.account_mode() != state.mode || proposal.as_of() > request.at {
        return Err(inconsistent(InputKind::Configuration));
    }
    let currency = state.equity.currency;
    let equity_values = [
        state.equity,
        state.equity_at_day_start,
        state.equity_at_week_start,
        state.high_water_mark,
    ];
    if equity_values
        .iter()
        .any(|m| m.currency != currency || m.amount <= Decimal::ZERO)
    {
        return Err(inconsistent(InputKind::Equity));
    }
    if state
        .open_risk
        .iter()
        .any(|item| item.amount.currency != currency || item.amount.amount < Decimal::ZERO)
    {
        return Err(inconsistent(InputKind::Positions));
    }
    let fx = &request.fx;
    if fx.base() != spec.currency || fx.quote() != currency || !fx.is_known_at(request.at) {
        return Err(inconsistent(InputKind::FxRates));
    }
    Ok(())
}
