//! Exact decimal newtypes for prices, quantities, money, ratios and FX rates
//! (spec §6.1, INV-13).
//!
//! Everything is `rust_decimal::Decimal`. Binary floating point never appears in
//! these types, and serialization writes decimals as strings so JSON consumers
//! cannot silently turn them into floats.

use std::fmt;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors from constructing or combining numeric values.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum NumError {
    /// Prices must be strictly positive.
    #[error("price must be greater than zero, got {0}")]
    NonPositivePrice(Decimal),
    /// Quantities cannot be negative; direction is carried by the trade action.
    #[error("quantity must not be negative, got {0}")]
    NegativeQuantity(Decimal),
    /// Ratios (fractions of equity, limits, shocks) cannot be negative.
    #[error("ratio must not be negative, got {0}")]
    NegativeRatio(Decimal),
    /// Two amounts in different currencies were combined.
    #[error("currency mismatch: expected {expected}, got {actual}")]
    CurrencyMismatch {
        /// The currency the operation required.
        expected: Currency,
        /// The currency that was supplied.
        actual: Currency,
    },
    /// A currency code was not 3 to 8 upper-case ASCII letters or digits.
    #[error("invalid currency code {0:?}")]
    InvalidCurrency(String),
    /// FX rates must be strictly positive, and exactly 1 between a currency and itself.
    #[error("invalid FX rate {rate} for {base}/{quote}")]
    InvalidFxRate {
        /// Base currency.
        base: Currency,
        /// Quote currency.
        quote: Currency,
        /// The rejected rate.
        rate: Decimal,
    },
    /// Decimal arithmetic overflowed or divided by zero.
    #[error("decimal arithmetic overflow or division by zero")]
    Overflow,
}

/// Turns a `checked_*` result into a `NumError::Overflow` on failure.
pub(crate) fn checked(value: Option<Decimal>) -> Result<Decimal, NumError> {
    value.ok_or(NumError::Overflow)
}

/// A strictly positive price in the instrument's quote currency.
///
/// Instruments whose prices can go negative are not supported; constructing
/// such a price fails, so inputs fail closed (INV-06).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "Decimal", into = "Decimal")]
pub struct Price(Decimal);

impl Price {
    /// Creates a price, rejecting zero and negative values.
    pub fn new(value: Decimal) -> Result<Self, NumError> {
        if value > Decimal::ZERO {
            Ok(Self(value))
        } else {
            Err(NumError::NonPositivePrice(value))
        }
    }

    /// The price as a decimal.
    #[must_use]
    pub const fn value(self) -> Decimal {
        self.0
    }
}

impl TryFrom<Decimal> for Price {
    type Error = NumError;

    fn try_from(value: Decimal) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<Price> for Decimal {
    fn from(price: Price) -> Self {
        price.0
    }
}

impl fmt::Display for Price {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

/// A non-negative quantity of instrument units.
///
/// Direction never lives in the sign of a quantity; it lives in the
/// [`TradeAction`](crate::action::TradeAction) (INV-12).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "Decimal", into = "Decimal")]
pub struct Quantity(Decimal);

impl Quantity {
    /// Zero units.
    pub const ZERO: Self = Self(Decimal::ZERO);

    /// Creates a quantity, rejecting negative values.
    pub fn new(value: Decimal) -> Result<Self, NumError> {
        if value >= Decimal::ZERO {
            Ok(Self(value))
        } else {
            Err(NumError::NegativeQuantity(value))
        }
    }

    /// The quantity as a decimal.
    #[must_use]
    pub const fn value(self) -> Decimal {
        self.0
    }

    /// Whether this quantity is zero.
    #[must_use]
    pub fn is_zero(self) -> bool {
        self.0.is_zero()
    }
}

impl TryFrom<Decimal> for Quantity {
    type Error = NumError;

    fn try_from(value: Decimal) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<Quantity> for Decimal {
    fn from(quantity: Quantity) -> Self {
        quantity.0
    }
}

impl fmt::Display for Quantity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

/// An ISO-4217-style currency code such as `INR`, `USD` or `USDT`.
///
/// Stored inline so it is `Copy`. Codes are 3 to 8 upper-case ASCII letters or digits.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Currency {
    bytes: [u8; 8],
    len: u8,
}

impl Currency {
    /// Indian rupee.
    pub const INR: Self = Self::from_static(b"INR");
    /// US dollar.
    pub const USD: Self = Self::from_static(b"USD");
    /// Tether (a USD stablecoin used as a crypto quote currency).
    pub const USDT: Self = Self::from_static(b"USDT");

    // Only for the constants above, whose codes are known to be valid.
    const fn from_static(code: &[u8]) -> Self {
        let mut bytes = [0_u8; 8];
        let mut i = 0;
        while i < code.len() {
            bytes[i] = code[i];
            i += 1;
        }
        // code.len() <= 8 for every constant above, so the cast cannot truncate.
        let len = code.len() as u8;
        Self { bytes, len }
    }

    /// Parses and validates a currency code.
    pub fn new(code: &str) -> Result<Self, NumError> {
        let valid_len = (3..=8).contains(&code.len());
        let valid_chars = code
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
        if !(valid_len && valid_chars) {
            return Err(NumError::InvalidCurrency(code.to_owned()));
        }
        let mut bytes = [0_u8; 8];
        bytes[..code.len()].copy_from_slice(code.as_bytes());
        let len =
            u8::try_from(code.len()).map_err(|_| NumError::InvalidCurrency(code.to_owned()))?;
        Ok(Self { bytes, len })
    }

    /// The currency code.
    #[must_use]
    pub fn code(&self) -> &str {
        let len = usize::from(self.len).min(self.bytes.len());
        // Always valid: construction only admits ASCII.
        std::str::from_utf8(&self.bytes[..len]).unwrap_or("")
    }
}

impl TryFrom<String> for Currency {
    type Error = NumError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(&value)
    }
}

impl From<Currency> for String {
    fn from(currency: Currency) -> Self {
        currency.code().to_owned()
    }
}

impl fmt::Display for Currency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

impl fmt::Debug for Currency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Currency({})", self.code())
    }
}

/// An amount of money in a specific currency.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Money {
    /// The amount; may be negative (for example a loss).
    pub amount: Decimal,
    /// The currency of `amount`.
    pub currency: Currency,
}

impl Money {
    /// Creates an amount of money.
    #[must_use]
    pub const fn new(amount: Decimal, currency: Currency) -> Self {
        Self { amount, currency }
    }

    /// Zero in the given currency.
    #[must_use]
    pub const fn zero(currency: Currency) -> Self {
        Self::new(Decimal::ZERO, currency)
    }

    /// Fails unless this amount is in `currency`.
    pub fn ensure_currency(self, currency: Currency) -> Result<Self, NumError> {
        if self.currency == currency {
            Ok(self)
        } else {
            Err(NumError::CurrencyMismatch {
                expected: currency,
                actual: self.currency,
            })
        }
    }

    /// Adds two amounts in the same currency.
    pub fn checked_add(self, other: Self) -> Result<Self, NumError> {
        let other = other.ensure_currency(self.currency)?;
        Ok(Self::new(
            checked(self.amount.checked_add(other.amount))?,
            self.currency,
        ))
    }

    /// Subtracts an amount in the same currency.
    pub fn checked_sub(self, other: Self) -> Result<Self, NumError> {
        let other = other.ensure_currency(self.currency)?;
        Ok(Self::new(
            checked(self.amount.checked_sub(other.amount))?,
            self.currency,
        ))
    }

    /// Multiplies the amount by a dimensionless factor.
    pub fn checked_scale(self, factor: Decimal) -> Result<Self, NumError> {
        Ok(Self::new(
            checked(self.amount.checked_mul(factor))?,
            self.currency,
        ))
    }
}

impl fmt::Display for Money {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.amount, self.currency)
    }
}

/// A non-negative fraction, for example `0.005` for 0.5% of equity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "Decimal", into = "Decimal")]
pub struct Ratio(Decimal);

impl Ratio {
    /// Zero.
    pub const ZERO: Self = Self(Decimal::ZERO);

    /// Creates a ratio from a fraction (`0.02` is 2%).
    pub fn new(fraction: Decimal) -> Result<Self, NumError> {
        if fraction >= Decimal::ZERO {
            Ok(Self(fraction))
        } else {
            Err(NumError::NegativeRatio(fraction))
        }
    }

    /// Creates a ratio from a percentage (`2` is 2%).
    pub fn from_percent(percent: Decimal) -> Result<Self, NumError> {
        Self::new(checked(percent.checked_div(Decimal::ONE_HUNDRED))?)
    }

    /// The ratio as a fraction.
    #[must_use]
    pub const fn value(self) -> Decimal {
        self.0
    }

    /// The ratio as a percentage.
    pub fn as_percent(self) -> Result<Decimal, NumError> {
        checked(self.0.checked_mul(Decimal::ONE_HUNDRED))
    }
}

impl TryFrom<Decimal> for Ratio {
    type Error = NumError;

    fn try_from(value: Decimal) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<Ratio> for Decimal {
    fn from(ratio: Ratio) -> Self {
        ratio.0
    }
}

/// Basis points (1 bp = 0.01%). May be negative, for example a price change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Bps(pub Decimal);

impl Bps {
    /// The value as a fraction (`25` bp is `0.0025`).
    pub fn as_fraction(self) -> Result<Decimal, NumError> {
        checked(self.0.checked_div(Decimal::from(10_000)))
    }
}

/// A point-in-time FX rate: one unit of `base` is worth `rate` units of `quote`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FxRate {
    base: Currency,
    quote: Currency,
    rate: Decimal,
    as_of: DateTime<Utc>,
}

impl FxRate {
    /// Creates a rate. It must be positive, and exactly 1 when `base == quote`.
    pub fn new(
        base: Currency,
        quote: Currency,
        rate: Decimal,
        as_of: DateTime<Utc>,
    ) -> Result<Self, NumError> {
        let valid = rate > Decimal::ZERO && (base != quote || rate == Decimal::ONE);
        if valid {
            Ok(Self {
                base,
                quote,
                rate,
                as_of,
            })
        } else {
            Err(NumError::InvalidFxRate { base, quote, rate })
        }
    }

    /// The identity rate for amounts already in `currency`.
    #[must_use]
    pub const fn identity(currency: Currency, as_of: DateTime<Utc>) -> Self {
        Self {
            base: currency,
            quote: currency,
            rate: Decimal::ONE,
            as_of,
        }
    }

    /// Base currency.
    #[must_use]
    pub const fn base(&self) -> Currency {
        self.base
    }

    /// Quote currency.
    #[must_use]
    pub const fn quote(&self) -> Currency {
        self.quote
    }

    /// Units of quote per unit of base.
    #[must_use]
    pub const fn rate(&self) -> Decimal {
        self.rate
    }

    /// When this rate was observed.
    #[must_use]
    pub const fn as_of(&self) -> DateTime<Utc> {
        self.as_of
    }

    /// Whether the rate was knowable at `at` (point-in-time rule, INV-09).
    #[must_use]
    pub fn is_known_at(&self, at: DateTime<Utc>) -> bool {
        self.as_of <= at
    }

    /// Converts an amount in the base currency into the quote currency.
    pub fn convert(&self, money: Money) -> Result<Money, NumError> {
        let money = money.ensure_currency(self.base)?;
        Ok(Money::new(
            checked(money.amount.checked_mul(self.rate))?,
            self.quote,
        ))
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use rust_decimal_macros::dec;

    use super::*;

    #[test]
    fn price_must_be_positive() {
        assert!(Price::new(dec!(0.05)).is_ok());
        assert_eq!(
            Price::new(Decimal::ZERO),
            Err(NumError::NonPositivePrice(Decimal::ZERO))
        );
        assert!(Price::new(dec!(-1)).is_err());
    }

    #[test]
    fn quantity_cannot_be_negative() {
        assert!(Quantity::new(Decimal::ZERO).is_ok());
        assert!(Quantity::new(dec!(-0.0001)).is_err());
    }

    #[test]
    fn currency_codes_are_validated() {
        assert_eq!(Currency::new("INR").unwrap(), Currency::INR);
        assert_eq!(Currency::USDT.code(), "USDT");
        assert!(Currency::new("inr").is_err());
        assert!(Currency::new("IN").is_err());
        assert!(Currency::new("TOOLONGCODE").is_err());
    }

    #[test]
    fn money_refuses_to_mix_currencies() {
        let a = Money::new(dec!(10), Currency::INR);
        let b = Money::new(dec!(5), Currency::USD);
        assert!(matches!(
            a.checked_add(b),
            Err(NumError::CurrencyMismatch { .. })
        ));
        assert_eq!(
            a.checked_sub(Money::new(dec!(2.5), Currency::INR)).unwrap(),
            Money::new(dec!(7.5), Currency::INR)
        );
    }

    #[test]
    fn decimals_serialize_as_strings() {
        let money = Money::new(dec!(1234.50), Currency::INR);
        let json = serde_json::to_string(&money).unwrap();
        assert_eq!(json, r#"{"amount":"1234.50","currency":"INR"}"#);
        assert_eq!(serde_json::from_str::<Money>(&json).unwrap(), money);
        assert!(serde_json::from_str::<Price>(r#""-1""#).is_err());
    }

    #[test]
    fn fx_rates_convert_and_validate() {
        let at = Utc.with_ymd_and_hms(2026, 1, 5, 10, 0, 0).unwrap();
        let usd_inr = FxRate::new(Currency::USD, Currency::INR, dec!(83.25), at).unwrap();
        let converted = usd_inr.convert(Money::new(dec!(2), Currency::USD)).unwrap();
        assert_eq!(converted, Money::new(dec!(166.50), Currency::INR));
        assert!(usd_inr.convert(Money::new(dec!(2), Currency::INR)).is_err());
        assert!(FxRate::new(Currency::USD, Currency::INR, Decimal::ZERO, at).is_err());
        assert!(FxRate::new(Currency::INR, Currency::INR, dec!(1.01), at).is_err());
        assert!(usd_inr.is_known_at(at));
        assert!(!usd_inr.is_known_at(at - chrono::Duration::seconds(1)));
    }

    #[test]
    fn ratios_and_bps() {
        assert_eq!(Ratio::from_percent(dec!(0.5)).unwrap().value(), dec!(0.005));
        assert!(Ratio::new(dec!(-0.01)).is_err());
        assert_eq!(Bps(dec!(25)).as_fraction().unwrap(), dec!(0.0025));
    }
}
