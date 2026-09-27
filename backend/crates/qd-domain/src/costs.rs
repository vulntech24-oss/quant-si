//! Versioned, data-driven transaction cost model (spec §5.2 "Foundation", §6.5, INV-08).
//!
//! Charge rates are data ([`CostScheduleData`], loaded from configuration),
//! never code. Every schedule carries a [`Verification`] record: rates that
//! have not been confirmed against the broker's and exchanges' official
//! schedules are marked unverified, and the Risk Gate refuses them for live
//! accounts.
//!
//! For each leg of the round trip:
//!
//! - `turnover = quantity × multiplier × price`
//! - brokerage `= min(rate × turnover, max_per_order)`
//! - each charge that applies to the leg's [`Transfer`]: `rate × turnover` or a
//!   flat amount per order
//! - GST `= gst_rate × (brokerage + charges marked gst)`
//!
//! Every amount is rounded to `rounding_dp` decimals (half away from zero) per
//! leg, as contract notes do. Lines are then summed over both legs.

use chrono::NaiveDate;
use rust_decimal::{Decimal, RoundingStrategy};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::action::{Side, Transfer};
use crate::economics::{CostEstimate, CostLine};
use crate::instrument::{AssetClass, InstrumentKind, InstrumentSpec, ProductType, Venue};
use crate::num::{Currency, Price, Quantity};
use crate::plan::PlanDefect;

/// Which transfers a charge applies to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppliesTo {
    /// Orders that acquire units.
    Purchase,
    /// Orders that dispose of units.
    Disposal,
    /// Every order.
    Both,
}

impl AppliesTo {
    const fn covers(self, transfer: Transfer) -> bool {
        matches!(
            (self, transfer),
            (Self::Both, _)
                | (Self::Purchase, Transfer::Purchase)
                | (Self::Disposal, Transfer::Disposal)
        )
    }
}

/// How a charge is computed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChargeBasis {
    /// A fraction of the leg's turnover (`0.001` is 0.1%).
    Turnover {
        /// The fraction.
        rate: Decimal,
    },
    /// A flat amount per executed order.
    PerOrder {
        /// The amount, in the schedule currency.
        amount: Decimal,
    },
}

/// One statutory or exchange charge.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChargeComponent {
    /// Line-item name, for example `stt` or `exchange_transaction`.
    pub name: String,
    /// Which orders pay it.
    pub applies_to: AppliesTo,
    /// How it is computed.
    pub basis: ChargeBasis,
    /// Whether GST is levied on it.
    pub gst: bool,
}

/// Broker commission: `min(rate × turnover, max_per_order)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrokerageRule {
    /// Fraction of turnover.
    pub rate: Decimal,
    /// Cap per executed order, if any.
    pub max_per_order: Option<Decimal>,
}

/// Whether a schedule's rates have been confirmed against official sources.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStatus {
    /// Confirmed against the broker's and exchanges' official schedules.
    Verified,
    /// Not yet confirmed. Usable for research and paper, never for live orders.
    Unverified,
}

/// Where a schedule's rates come from and whether they are confirmed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verification {
    /// Status.
    pub status: VerificationStatus,
    /// When the rates were last checked.
    pub checked_on: NaiveDate,
    /// Where they were checked.
    pub sources: Vec<String>,
    /// Anything a reader must know.
    pub note: String,
}

/// The editable form of a cost schedule. Validate it with [`CostSchedule::new`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostScheduleData {
    /// Schedule id, for example `zerodha-nse-equity-delivery`.
    pub id: String,
    /// Version. A rate change is a new version.
    pub version: u32,
    /// First trade date the version applies to.
    pub effective_from: NaiveDate,
    /// First trade date it no longer applies to (exclusive).
    pub effective_to: Option<NaiveDate>,
    /// Venue it applies to.
    pub venue: Venue,
    /// Instrument kinds it applies to.
    pub instrument_kinds: Vec<InstrumentKind>,
    /// Asset classes it applies to.
    pub asset_classes: Vec<AssetClass>,
    /// Product it applies to.
    pub product: ProductType,
    /// Currency of every amount.
    pub currency: Currency,
    /// Broker commission.
    pub brokerage: BrokerageRule,
    /// Statutory and exchange charges.
    pub charges: Vec<ChargeComponent>,
    /// GST rate on brokerage and GST-bearing charges.
    pub gst_rate: Decimal,
    /// Decimals each amount is rounded to.
    pub rounding_dp: u32,
    /// Verification record.
    pub verification: Verification,
}

/// A set of schedules, as stored in a configuration file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostScheduleSet {
    /// The schedules.
    pub schedules: Vec<CostScheduleData>,
}

/// Why a cost schedule or a cost quote is invalid.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CostError {
    /// The schedule data is malformed.
    #[error("invalid cost schedule {id}: {detail}")]
    InvalidSchedule {
        /// Schedule id.
        id: String,
        /// What is wrong.
        detail: String,
    },
    /// No schedule covers the instrument, product and date. Fails closed.
    #[error("no cost schedule covers {symbol} ({product:?}) on {date}")]
    NoSchedule {
        /// Instrument symbol.
        symbol: String,
        /// Product.
        product: ProductType,
        /// Trade date.
        date: NaiveDate,
    },
    /// More than one schedule covers the request.
    #[error("{count} cost schedules cover {symbol} ({product:?}) on {date}")]
    AmbiguousSchedule {
        /// Instrument symbol.
        symbol: String,
        /// Product.
        product: ProductType,
        /// Trade date.
        date: NaiveDate,
        /// How many matched.
        count: usize,
    },
    /// Costs for zero units are meaningless.
    #[error("cost quantity must be greater than zero")]
    ZeroQuantity,
    /// The schedule currency differs from the instrument currency.
    #[error("schedule currency {schedule} differs from instrument currency {instrument}")]
    CurrencyMismatch {
        /// Schedule currency.
        schedule: Currency,
        /// Instrument currency.
        instrument: Currency,
    },
    /// Arithmetic overflowed or the estimate was rejected.
    #[error(transparent)]
    Estimate(#[from] PlanDefect),
}

/// A validated cost schedule.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CostSchedule(CostScheduleData);

const RESERVED_LINES: [&str; 2] = ["brokerage", "gst"];

fn is_fraction(value: Decimal) -> bool {
    (Decimal::ZERO..Decimal::ONE).contains(&value)
}

fn arith(value: Option<Decimal>) -> Result<Decimal, CostError> {
    value.ok_or(CostError::Estimate(PlanDefect::ArithmeticOverflow))
}

impl CostSchedule {
    /// Validates schedule data.
    pub fn new(data: CostScheduleData) -> Result<Self, CostError> {
        let invalid = |detail: &str| CostError::InvalidSchedule {
            id: data.id.clone(),
            detail: detail.to_owned(),
        };
        if data.id.trim().is_empty() {
            return Err(invalid("id is empty"));
        }
        if data
            .effective_to
            .is_some_and(|to| to <= data.effective_from)
        {
            return Err(invalid("effective_to must be after effective_from"));
        }
        if data.instrument_kinds.is_empty() || data.asset_classes.is_empty() {
            return Err(invalid(
                "instrument_kinds and asset_classes must not be empty",
            ));
        }
        if !is_fraction(data.brokerage.rate) || !is_fraction(data.gst_rate) {
            return Err(invalid(
                "brokerage and GST rates must be fractions in [0, 1)",
            ));
        }
        if data
            .brokerage
            .max_per_order
            .is_some_and(|m| m < Decimal::ZERO)
        {
            return Err(invalid("brokerage cap must not be negative"));
        }
        if data.rounding_dp > 6 {
            return Err(invalid("rounding_dp must be at most 6"));
        }
        let mut names: Vec<&str> = Vec::new();
        for charge in &data.charges {
            let name = charge.name.trim();
            if name.is_empty() || RESERVED_LINES.contains(&name) || names.contains(&name) {
                return Err(invalid(
                    "charge names must be non-empty, unique and not reserved",
                ));
            }
            names.push(name);
            let valid = match charge.basis {
                ChargeBasis::Turnover { rate } => is_fraction(rate),
                ChargeBasis::PerOrder { amount } => amount >= Decimal::ZERO,
            };
            if !valid {
                return Err(invalid(
                    "charge rates must be fractions in [0, 1) and amounts >= 0",
                ));
            }
        }
        if data.verification.sources.is_empty() {
            return Err(invalid("verification needs at least one source"));
        }
        Ok(Self(data))
    }

    /// The schedule data.
    #[must_use]
    pub const fn data(&self) -> &CostScheduleData {
        &self.0
    }

    /// `id@version`, recorded as the cost-model version on every estimate.
    #[must_use]
    pub fn model_version(&self) -> String {
        format!("{}@{}", self.0.id, self.0.version)
    }

    /// Whether the rates are confirmed.
    #[must_use]
    pub fn is_verified(&self) -> bool {
        self.0.verification.status == VerificationStatus::Verified
    }

    /// Whether the schedule covers this instrument, product and trade date.
    #[must_use]
    pub fn covers(&self, spec: &InstrumentSpec, product: ProductType, date: NaiveDate) -> bool {
        let d = &self.0;
        d.venue == spec.venue
            && d.product == product
            && d.instrument_kinds.contains(&spec.kind)
            && d.asset_classes.contains(&spec.asset_class)
            && date >= d.effective_from
            && d.effective_to.is_none_or(|to| date < to)
    }

    fn round(&self, amount: Decimal) -> Decimal {
        amount.round_dp_with_strategy(self.0.rounding_dp, RoundingStrategy::MidpointAwayFromZero)
    }

    /// Charges for one order, as `(line name, amount)` pairs.
    fn leg(
        &self,
        transfer: Transfer,
        turnover: Decimal,
    ) -> Result<Vec<(String, Decimal)>, CostError> {
        let d = &self.0;
        let mut brokerage = arith(d.brokerage.rate.checked_mul(turnover))?;
        if let Some(cap) = d.brokerage.max_per_order {
            brokerage = brokerage.min(cap);
        }
        let brokerage = self.round(brokerage);
        let mut gst_base = brokerage;
        let mut lines = vec![("brokerage".to_owned(), brokerage)];
        for charge in d.charges.iter().filter(|c| c.applies_to.covers(transfer)) {
            let amount = match charge.basis {
                ChargeBasis::Turnover { rate } => arith(rate.checked_mul(turnover))?,
                ChargeBasis::PerOrder { amount } => amount,
            };
            let amount = self.round(amount);
            if charge.gst {
                gst_base = arith(gst_base.checked_add(amount))?;
            }
            lines.push((charge.name.clone(), amount));
        }
        let gst = self.round(arith(d.gst_rate.checked_mul(gst_base))?);
        lines.push(("gst".to_owned(), gst));
        Ok(lines)
    }
}

/// What to price: one round trip of `quantity` units.
#[derive(Clone, Copy, Debug)]
pub struct CostRequest<'a> {
    /// The instrument.
    pub spec: &'a InstrumentSpec,
    /// Product.
    pub product: ProductType,
    /// Side of the position.
    pub side: Side,
    /// Units per leg.
    pub quantity: Quantity,
    /// Entry price.
    pub entry: Price,
    /// Exit price. Use the plan level with the larger turnover to stay conservative.
    pub exit: Price,
    /// Trade date on the venue calendar.
    pub trade_date: NaiveDate,
}

/// A cost estimate and whether its rates are verified.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CostQuote {
    /// The estimate, with line items and the schedule version.
    pub estimate: CostEstimate,
    /// Whether the schedule's rates are confirmed.
    pub verified: bool,
}

/// The shared cost model port. The same implementation serves backtest, paper and live.
pub trait CostModel: Send + Sync {
    /// Prices one round trip.
    fn quote(&self, request: &CostRequest<'_>) -> Result<CostQuote, CostError>;
}

/// A cost model backed by validated schedules.
#[derive(Clone, Debug, Default)]
pub struct ScheduleCostModel {
    schedules: Vec<CostSchedule>,
}

impl ScheduleCostModel {
    /// Validates every schedule.
    pub fn new(set: CostScheduleSet) -> Result<Self, CostError> {
        let schedules = set
            .schedules
            .into_iter()
            .map(CostSchedule::new)
            .collect::<Result<Vec<_>, _>>()?;
        for (i, a) in schedules.iter().enumerate() {
            let duplicate = schedules[i + 1..]
                .iter()
                .any(|b| a.0.id == b.0.id && a.0.version == b.0.version);
            if duplicate {
                return Err(CostError::InvalidSchedule {
                    id: a.0.id.clone(),
                    detail: "duplicate id and version".to_owned(),
                });
            }
        }
        Ok(Self { schedules })
    }

    /// The schedules.
    #[must_use]
    pub fn schedules(&self) -> &[CostSchedule] {
        &self.schedules
    }

    fn schedule_for(&self, request: &CostRequest<'_>) -> Result<&CostSchedule, CostError> {
        let matches: Vec<&CostSchedule> = self
            .schedules
            .iter()
            .filter(|s| s.covers(request.spec, request.product, request.trade_date))
            .collect();
        match matches.as_slice() {
            [one] => Ok(one),
            [] => Err(CostError::NoSchedule {
                symbol: request.spec.symbol.clone(),
                product: request.product,
                date: request.trade_date,
            }),
            many => Err(CostError::AmbiguousSchedule {
                symbol: request.spec.symbol.clone(),
                product: request.product,
                date: request.trade_date,
                count: many.len(),
            }),
        }
    }
}

impl CostModel for ScheduleCostModel {
    fn quote(&self, request: &CostRequest<'_>) -> Result<CostQuote, CostError> {
        if request.quantity.is_zero() {
            return Err(CostError::ZeroQuantity);
        }
        let schedule = self.schedule_for(request)?;
        if schedule.0.currency != request.spec.currency {
            return Err(CostError::CurrencyMismatch {
                schedule: schedule.0.currency,
                instrument: request.spec.currency,
            });
        }
        let units = arith(
            request
                .quantity
                .value()
                .checked_mul(request.spec.multiplier),
        )?;
        let legs = [
            (request.side.entry().action().transfer(), request.entry),
            (request.side.exit().action().transfer(), request.exit),
        ];
        let mut lines: Vec<CostLine> = Vec::new();
        for (transfer, price) in legs {
            let turnover = arith(units.checked_mul(price.value()))?;
            for (name, amount) in schedule.leg(transfer, turnover)? {
                match lines.iter_mut().find(|line| line.name == name) {
                    Some(line) => line.amount = arith(line.amount.checked_add(amount))?,
                    None => lines.push(CostLine { name, amount }),
                }
            }
        }
        let estimate = CostEstimate::new(
            schedule.model_version(),
            schedule.0.currency,
            request.quantity,
            lines,
        )?;
        Ok(CostQuote {
            estimate,
            verified: schedule.is_verified(),
        })
    }
}
