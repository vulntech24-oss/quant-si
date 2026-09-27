//! Per-unit economics, outcome probabilities and expected value (spec §6.5).
//!
//! Every amount here is in the instrument's currency for one unit of quantity.
//! Price distances are therefore multiplied by the contract multiplier:
//! `risk_gross = (entry − stop) × multiplier`. For cash equities the multiplier
//! is 1 and the formulas are exactly the spec's; for futures (for example one
//! MCX crude lot of 100 barrels) the multiplier keeps risk and reward in money
//! per unit (ADR 0003).

use rust_decimal::Decimal;
use serde::Serialize;

use crate::instrument::InstrumentSpec;
use crate::num::{Currency, Quantity};
use crate::plan::{PlanDefect, TradePlan, overflow};

/// One line of a round-trip cost estimate (brokerage, taxes, exchange fees, …).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CostLine {
    /// What the cost is.
    pub name: String,
    /// Round-trip amount at the estimate's reference quantity, in the instrument currency.
    pub amount: Decimal,
}

/// Round-trip costs from the shared cost model, with every line item.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CostEstimate {
    model_version: String,
    currency: Currency,
    reference_quantity: Quantity,
    lines: Vec<CostLine>,
    total: Decimal,
    per_unit: Decimal,
}

impl CostEstimate {
    /// Validates a cost estimate made for `reference_quantity` units.
    pub fn new(
        model_version: impl Into<String>,
        currency: Currency,
        reference_quantity: Quantity,
        lines: Vec<CostLine>,
    ) -> Result<Self, PlanDefect> {
        let model_version = model_version.into();
        let invalid = |detail: &str| PlanDefect::InvalidCosts {
            detail: detail.to_owned(),
        };
        if model_version.trim().is_empty() {
            return Err(invalid("cost model version is empty"));
        }
        if reference_quantity.is_zero() {
            return Err(invalid("reference quantity is zero"));
        }
        let mut total = Decimal::ZERO;
        for line in &lines {
            if line.amount < Decimal::ZERO {
                return Err(invalid("a cost line is negative"));
            }
            total = overflow(total.checked_add(line.amount))?;
        }
        let per_unit = overflow(total.checked_div(reference_quantity.value()))?;
        Ok(Self {
            model_version,
            currency,
            reference_quantity,
            lines,
            total,
            per_unit,
        })
    }

    /// Cost model version.
    #[must_use]
    pub fn model_version(&self) -> &str {
        &self.model_version
    }

    /// Currency of every amount.
    #[must_use]
    pub const fn currency(&self) -> Currency {
        self.currency
    }

    /// The quantity the estimate was made for.
    #[must_use]
    pub const fn reference_quantity(&self) -> Quantity {
        self.reference_quantity
    }

    /// Line items.
    #[must_use]
    pub fn lines(&self) -> &[CostLine] {
        &self.lines
    }

    /// Total round-trip cost at the reference quantity.
    #[must_use]
    pub const fn total(&self) -> Decimal {
        self.total
    }

    /// Total round-trip cost per unit of quantity.
    #[must_use]
    pub const fn per_unit(&self) -> Decimal {
        self.per_unit
    }
}

/// Modeled adverse slippage on stop exits, including a gap allowance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SlippageAssumption {
    model_version: String,
    stop_points: Decimal,
}

impl SlippageAssumption {
    /// Validates a slippage assumption. `stop_points` is in price points and must be ≥ 0.
    pub fn new(model_version: impl Into<String>, stop_points: Decimal) -> Result<Self, PlanDefect> {
        let model_version = model_version.into();
        if model_version.trim().is_empty() {
            return Err(PlanDefect::InvalidSlippage {
                detail: "slippage model version is empty".to_owned(),
            });
        }
        if stop_points < Decimal::ZERO {
            return Err(PlanDefect::InvalidSlippage {
                detail: "stop slippage is negative".to_owned(),
            });
        }
        Ok(Self {
            model_version,
            stop_points,
        })
    }

    /// Slippage model version.
    #[must_use]
    pub fn model_version(&self) -> &str {
        &self.model_version
    }

    /// Adverse slippage on a stop exit, in price points.
    #[must_use]
    pub const fn stop_points(&self) -> Decimal {
        self.stop_points
    }
}

/// Gross and net reward and risk for one unit of quantity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct UnitEconomics {
    currency: Currency,
    risk_gross: Decimal,
    reward_gross: Decimal,
    costs_per_unit: Decimal,
    stop_slippage: Decimal,
    risk_net: Decimal,
    reward_net: Decimal,
    rr_net: Decimal,
}

impl UnitEconomics {
    /// Computes per-unit economics for a plan:
    ///
    /// - `risk_net = risk_gross + costs_per_unit + stop_slippage`
    /// - `reward_net = reward_gross − costs_per_unit` (must be > 0)
    /// - `rr_net = reward_net / risk_net`
    pub fn compute(
        plan: &TradePlan,
        spec: &InstrumentSpec,
        costs: &CostEstimate,
        slippage: &SlippageAssumption,
    ) -> Result<Self, PlanDefect> {
        if costs.currency() != spec.currency {
            return Err(PlanDefect::CurrencyMismatch);
        }
        let multiplier = spec.multiplier;
        let risk_gross = overflow(plan.risk_points().checked_mul(multiplier))?;
        let reward_gross = overflow(plan.reward_points().checked_mul(multiplier))?;
        let costs_per_unit = costs.per_unit();
        let stop_slippage = overflow(slippage.stop_points().checked_mul(multiplier))?;

        let risk_net = overflow(
            risk_gross
                .checked_add(costs_per_unit)
                .and_then(|r| r.checked_add(stop_slippage)),
        )?;
        let reward_net = overflow(reward_gross.checked_sub(costs_per_unit))?;
        if reward_net <= Decimal::ZERO {
            return Err(PlanDefect::NonPositiveNetReward);
        }
        // risk_net > 0: risk_gross > 0 (validated plan) and the other terms are >= 0.
        let rr_net = overflow(reward_net.checked_div(risk_net))?;
        Ok(Self {
            currency: spec.currency,
            risk_gross,
            reward_gross,
            costs_per_unit,
            stop_slippage,
            risk_net,
            reward_net,
            rr_net,
        })
    }

    /// Currency of every amount.
    #[must_use]
    pub const fn currency(&self) -> Currency {
        self.currency
    }

    /// Loss per unit if the stop fills exactly, before costs.
    #[must_use]
    pub const fn risk_gross(&self) -> Decimal {
        self.risk_gross
    }

    /// Profit per unit at the target, before costs.
    #[must_use]
    pub const fn reward_gross(&self) -> Decimal {
        self.reward_gross
    }

    /// Round-trip costs per unit.
    #[must_use]
    pub const fn costs_per_unit(&self) -> Decimal {
        self.costs_per_unit
    }

    /// Modeled adverse stop slippage per unit.
    #[must_use]
    pub const fn stop_slippage(&self) -> Decimal {
        self.stop_slippage
    }

    /// Loss per unit at the stop after costs and slippage. One R.
    #[must_use]
    pub const fn risk_net(&self) -> Decimal {
        self.risk_net
    }

    /// Profit per unit at the target after costs.
    #[must_use]
    pub const fn reward_net(&self) -> Decimal {
        self.reward_net
    }

    /// Net reward-to-risk ratio.
    #[must_use]
    pub const fn rr_net(&self) -> Decimal {
        self.rr_net
    }
}

/// Probabilities for how this exact plan ends: target first, stop first or time exit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OutcomeProbabilities {
    p_target: Decimal,
    p_stop: Decimal,
    p_time: Decimal,
    source: String,
    evidence_count: u32,
}

impl OutcomeProbabilities {
    /// Validates the probabilities: each in `[0, 1]`, summing to exactly 1.
    pub fn new(
        p_target: Decimal,
        p_stop: Decimal,
        p_time: Decimal,
        source: impl Into<String>,
        evidence_count: u32,
    ) -> Result<Self, PlanDefect> {
        let invalid = |detail: &str| PlanDefect::InvalidProbabilities {
            detail: detail.to_owned(),
        };
        let source = source.into();
        if source.trim().is_empty() {
            return Err(invalid("probability source is empty"));
        }
        let in_range = |p: Decimal| (Decimal::ZERO..=Decimal::ONE).contains(&p);
        if !(in_range(p_target) && in_range(p_stop) && in_range(p_time)) {
            return Err(invalid("a probability is outside [0, 1]"));
        }
        let sum = overflow(
            p_target
                .checked_add(p_stop)
                .and_then(|s| s.checked_add(p_time)),
        )?;
        if sum != Decimal::ONE {
            return Err(invalid("probabilities do not sum to 1"));
        }
        Ok(Self {
            p_target,
            p_stop,
            p_time,
            source,
            evidence_count,
        })
    }

    /// Probability the target is hit first.
    #[must_use]
    pub const fn p_target(&self) -> Decimal {
        self.p_target
    }

    /// Probability the stop is hit first.
    #[must_use]
    pub const fn p_stop(&self) -> Decimal {
        self.p_stop
    }

    /// Probability the time exit comes first.
    #[must_use]
    pub const fn p_time(&self) -> Decimal {
        self.p_time
    }

    /// Where the probabilities come from (method and version).
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Number of comparable setups behind the probabilities.
    #[must_use]
    pub const fn evidence_count(&self) -> u32 {
        self.evidence_count
    }
}

/// Expected value of the plan per unit and in R.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ExpectedValue {
    per_unit: Decimal,
    in_r: Decimal,
}

impl ExpectedValue {
    /// `EV = p_target × reward_net − p_stop × risk_net + p_time × time_exit_pnl_per_unit`
    /// and `EV_R = EV / risk_net`.
    ///
    /// `time_exit_pnl_per_unit` is `E[net P&L per unit | time exit]` in the instrument currency.
    pub fn compute(
        economics: &UnitEconomics,
        probabilities: &OutcomeProbabilities,
        time_exit_pnl_per_unit: Decimal,
    ) -> Result<Self, PlanDefect> {
        let win = overflow(probabilities.p_target().checked_mul(economics.reward_net()))?;
        let loss = overflow(probabilities.p_stop().checked_mul(economics.risk_net()))?;
        let time = overflow(probabilities.p_time().checked_mul(time_exit_pnl_per_unit))?;
        let per_unit = overflow(win.checked_sub(loss).and_then(|v| v.checked_add(time)))?;
        let in_r = overflow(per_unit.checked_div(economics.risk_net()))?;
        Ok(Self { per_unit, in_r })
    }

    /// Expected value per unit, in the instrument currency.
    #[must_use]
    pub const fn per_unit(&self) -> Decimal {
        self.per_unit
    }

    /// Expected value in R (multiples of `risk_net`).
    #[must_use]
    pub const fn in_r(&self) -> Decimal {
        self.in_r
    }
}
