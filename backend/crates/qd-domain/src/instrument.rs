//! Instrument specifications (spec §6.2).
//!
//! An [`InstrumentSpec`] is data, never hardcoded. Specs are versioned with
//! effective dates. The spec owns the tick and quantity rounding rules that
//! every level and quantity passes through before economics or sizing are
//! finalized (INV-13).

use std::ops::Deref;

use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::action::Side;
use crate::ids::InstrumentId;
use crate::num::{Currency, NumError, Price, Quantity};

/// Trading venue.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Venue {
    /// National Stock Exchange of India, cash segment.
    Nse,
    /// BSE, cash segment.
    Bse,
    /// NSE futures and options segment.
    Nfo,
    /// Multi Commodity Exchange of India.
    Mcx,
    /// A crypto venue, identified by a code such as the exchange name.
    Crypto {
        /// Venue code.
        exchange: String,
    },
}

/// Broad asset class.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetClass {
    /// Stocks and equity ETFs.
    Equity,
    /// Gold and silver, through ETFs or futures.
    PreciousMetal,
    /// Crude oil and other energy contracts.
    Energy,
    /// Cryptocurrencies.
    Crypto,
}

/// Instrument kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstrumentKind {
    /// A listed share.
    CashEquity,
    /// An exchange-traded fund.
    Etf,
    /// A futures contract. Always has an expiry.
    Future,
    /// A spot instrument (for example a crypto pair).
    Spot,
}

/// How a venue or broker can protect an open position.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtectionMode {
    /// Broker-side one-cancels-other stop and target.
    BrokerOco,
    /// Broker-side stop only; targets are managed by QuantDesk.
    BrokerStopOnly,
    /// No broker-side protection is available.
    Unavailable,
}

/// Broker-neutral product type. Broker adapters map these to broker codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProductType {
    /// Cash-segment delivery (held overnight in the demat account).
    Delivery,
    /// Carry-forward derivatives position.
    Margin,
    /// Intraday position, squared off the same day.
    Intraday,
    /// Crypto spot.
    Spot,
}

/// Order type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderType {
    /// Market order.
    Market,
    /// Limit order.
    Limit,
    /// Stop order that becomes a market order when triggered.
    StopMarket,
    /// Stop order that becomes a limit order when triggered.
    StopLimit,
}

/// Order validity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Validity {
    /// Valid for the trading day.
    Day,
    /// Immediate or cancel.
    ImmediateOrCancel,
    /// Good till cancelled.
    GoodTillCancelled,
}

/// What an instrument allows. Checked by the decision core and the Order Gateway.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    /// Whether a short position may be held overnight.
    pub can_short_overnight: bool,
    /// Whether market orders are accepted.
    pub supports_market_orders: bool,
    /// Whether market orders must carry a market-protection limit.
    pub requires_market_protection: bool,
    /// Available protection modes, best first.
    pub protection_modes: Vec<ProtectionMode>,
    /// Allowed products.
    pub products: Vec<ProductType>,
    /// Allowed order types.
    pub order_types: Vec<OrderType>,
    /// Allowed validities.
    pub validities: Vec<Validity>,
}

/// Identifier of a venue trading calendar.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CalendarId(pub String);

/// Name of a correlated-risk bucket used by open-risk caps.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CorrelationBucket(pub String);

/// How one broker refers to the instrument.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrokerRef {
    /// Broker code, for example `kite`.
    pub broker: String,
    /// The broker's trading symbol.
    pub symbol: String,
    /// The broker's instrument token, if it has one.
    pub token: Option<String>,
}

/// Rounding direction for tick and quantity rounding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rounding {
    /// Toward negative infinity.
    Down,
    /// Toward positive infinity.
    Up,
}

/// Why an instrument spec or a value checked against it is invalid.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum InstrumentError {
    /// The symbol is empty.
    #[error("instrument symbol is empty")]
    EmptySymbol,
    /// A contract term that must be positive is not.
    #[error("{field} must be greater than zero, got {value}")]
    NonPositiveTerm {
        /// Name of the term.
        field: &'static str,
        /// The rejected value.
        value: Decimal,
    },
    /// The quantity step is not a whole number of lots.
    #[error("quantity step {step} is not a whole multiple of lot size {lot}")]
    StepNotMultipleOfLot {
        /// Quantity step.
        step: Decimal,
        /// Lot size.
        lot: Decimal,
    },
    /// The minimum quantity is not a whole number of steps.
    #[error("minimum quantity {min} is not a whole multiple of quantity step {step}")]
    MinNotMultipleOfStep {
        /// Minimum quantity.
        min: Decimal,
        /// Quantity step.
        step: Decimal,
    },
    /// Futures need an expiry and nothing else may have one.
    #[error("expiry must be set for futures and only for futures")]
    ExpiryMismatch,
    /// The effective range is empty.
    #[error("effective_to must be after effective_from")]
    EmptyEffectiveRange,
    /// A capability list that must not be empty is empty.
    #[error("capability list {0} is empty")]
    EmptyCapabilityList(&'static str),
    /// A numeric value could not be built.
    #[error(transparent)]
    Num(#[from] NumError),
}

/// The editable form of an instrument spec. Validate it with [`InstrumentSpec::new`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstrumentSpecData {
    /// Instrument id, stable across spec versions.
    pub id: InstrumentId,
    /// Spec version for this instrument.
    pub version: u32,
    /// First venue-calendar date on which this version applies.
    pub effective_from: NaiveDate,
    /// First date on which this version no longer applies (exclusive), if superseded.
    pub effective_to: Option<NaiveDate>,
    /// Exchange symbol.
    pub symbol: String,
    /// Venue.
    pub venue: Venue,
    /// Asset class.
    pub asset_class: AssetClass,
    /// Instrument kind.
    pub kind: InstrumentKind,
    /// Underlying, for derivatives and ETFs.
    pub underlying: Option<String>,
    /// Quote currency.
    pub currency: Currency,
    /// Minimum price increment.
    pub tick_size: Decimal,
    /// Units per exchange lot.
    pub lot_size: Decimal,
    /// Currency value of a one-point price move for one unit of quantity.
    pub multiplier: Decimal,
    /// Increment in which order quantities are expressed. A whole number of lots.
    pub quantity_step: Decimal,
    /// Smallest order quantity. A whole number of steps.
    pub min_quantity: Decimal,
    /// Expiry date, for futures.
    pub expiry: Option<NaiveDate>,
    /// Venue trading calendar.
    pub calendar_id: CalendarId,
    /// Correlated-risk bucket.
    pub correlation_bucket: CorrelationBucket,
    /// Broker references.
    pub broker_refs: Vec<BrokerRef>,
    /// Capabilities.
    pub capabilities: Capabilities,
}

/// A validated, immutable instrument spec. Read its fields through `Deref`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "InstrumentSpecData", into = "InstrumentSpecData")]
pub struct InstrumentSpec(InstrumentSpecData);

impl Deref for InstrumentSpec {
    type Target = InstrumentSpecData;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl TryFrom<InstrumentSpecData> for InstrumentSpec {
    type Error = InstrumentError;

    fn try_from(data: InstrumentSpecData) -> Result<Self, Self::Error> {
        Self::new(data)
    }
}

impl From<InstrumentSpec> for InstrumentSpecData {
    fn from(spec: InstrumentSpec) -> Self {
        spec.0
    }
}

fn is_multiple_of(value: Decimal, step: Decimal) -> bool {
    value.checked_rem(step).is_some_and(|r| r.is_zero())
}

/// Rounds `value` to a whole multiple of `step` in the given direction.
pub(crate) fn round_to_step(
    value: Decimal,
    step: Decimal,
    direction: Rounding,
) -> Result<Decimal, NumError> {
    let units = value.checked_div(step).ok_or(NumError::Overflow)?;
    let whole = match direction {
        Rounding::Down => units.floor(),
        Rounding::Up => units.ceil(),
    };
    whole.checked_mul(step).ok_or(NumError::Overflow)
}

impl InstrumentSpec {
    /// Validates the data and wraps it.
    pub fn new(data: InstrumentSpecData) -> Result<Self, InstrumentError> {
        if data.symbol.trim().is_empty() {
            return Err(InstrumentError::EmptySymbol);
        }
        for (field, value) in [
            ("tick_size", data.tick_size),
            ("lot_size", data.lot_size),
            ("multiplier", data.multiplier),
            ("quantity_step", data.quantity_step),
            ("min_quantity", data.min_quantity),
        ] {
            if value <= Decimal::ZERO {
                return Err(InstrumentError::NonPositiveTerm { field, value });
            }
        }
        if !is_multiple_of(data.quantity_step, data.lot_size) {
            return Err(InstrumentError::StepNotMultipleOfLot {
                step: data.quantity_step,
                lot: data.lot_size,
            });
        }
        if !is_multiple_of(data.min_quantity, data.quantity_step) {
            return Err(InstrumentError::MinNotMultipleOfStep {
                min: data.min_quantity,
                step: data.quantity_step,
            });
        }
        if (data.kind == InstrumentKind::Future) != data.expiry.is_some() {
            return Err(InstrumentError::ExpiryMismatch);
        }
        if data
            .effective_to
            .is_some_and(|to| to <= data.effective_from)
        {
            return Err(InstrumentError::EmptyEffectiveRange);
        }
        let caps = &data.capabilities;
        for (name, empty) in [
            ("protection_modes", caps.protection_modes.is_empty()),
            ("products", caps.products.is_empty()),
            ("order_types", caps.order_types.is_empty()),
            ("validities", caps.validities.is_empty()),
        ] {
            if empty {
                return Err(InstrumentError::EmptyCapabilityList(name));
            }
        }
        Ok(Self(data))
    }

    /// Whether this spec version applies on the given venue-calendar date.
    #[must_use]
    pub fn is_effective_on(&self, date: NaiveDate) -> bool {
        date >= self.effective_from && self.effective_to.is_none_or(|to| date < to)
    }

    /// Rounds a price level to the tick size in the given direction.
    pub fn round_price(&self, value: Decimal, direction: Rounding) -> Result<Price, NumError> {
        Price::new(round_to_step(value, self.tick_size, direction)?)
    }

    /// Whether a price is a whole number of ticks.
    #[must_use]
    pub fn is_tick_aligned(&self, price: Price) -> bool {
        is_multiple_of(price.value(), self.tick_size)
    }

    /// Rounds a non-negative quantity down to the quantity step (sizing step 2).
    pub fn round_quantity_down(&self, value: Decimal) -> Result<Quantity, NumError> {
        let value = Quantity::new(value)?.value();
        Quantity::new(round_to_step(value, self.quantity_step, Rounding::Down)?)
    }

    /// Whether a quantity is a valid order size: a whole number of steps, at least the minimum.
    #[must_use]
    pub fn is_valid_order_quantity(&self, quantity: Quantity) -> bool {
        quantity.value() >= self.min_quantity
            && is_multiple_of(quantity.value(), self.quantity_step)
    }

    /// Whether a position on this side may be held overnight.
    ///
    /// V1 trades last days to weeks, so every position is held overnight.
    #[must_use]
    pub fn permits_overnight(&self, side: Side) -> bool {
        match side {
            Side::Long => true,
            Side::Short => self.capabilities.can_short_overnight,
        }
    }
}
