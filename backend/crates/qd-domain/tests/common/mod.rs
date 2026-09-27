//! Shared test fixtures. The instruments are test data, not real contract terms.

// Test code: a failed unwrap is a failed test. The workspace-wide unwrap/expect
// ban targets runtime code, and clippy's allow-unwrap-in-tests does not reach
// helper functions in integration-test files.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(dead_code)] // each test binary uses a different subset of these fixtures

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use qd_domain::ids::InstrumentId;
use qd_domain::instrument::{
    AssetClass, BrokerRef, CalendarId, Capabilities, CorrelationBucket, InstrumentKind,
    InstrumentSpec, InstrumentSpecData, OrderType, ProductType, ProtectionMode, Validity, Venue,
};
use qd_domain::num::Currency;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

pub fn at(hour: u32, minute: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 3, 16, hour, minute, 0).unwrap()
}

pub fn date() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 3, 16).unwrap()
}

fn base(
    symbol: &str,
    venue: Venue,
    asset_class: AssetClass,
    kind: InstrumentKind,
    currency: Currency,
) -> InstrumentSpecData {
    InstrumentSpecData {
        id: InstrumentId::new_at(at(0, 0)),
        version: 1,
        effective_from: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
        effective_to: None,
        symbol: symbol.to_owned(),
        venue,
        asset_class,
        kind,
        underlying: None,
        currency,
        tick_size: dec!(0.05),
        lot_size: Decimal::ONE,
        multiplier: Decimal::ONE,
        quantity_step: Decimal::ONE,
        min_quantity: Decimal::ONE,
        expiry: None,
        calendar_id: CalendarId("nse".to_owned()),
        correlation_bucket: CorrelationBucket("india_equity".to_owned()),
        broker_refs: vec![BrokerRef {
            broker: "test".to_owned(),
            symbol: symbol.to_owned(),
            token: None,
        }],
        capabilities: Capabilities {
            can_short_overnight: false,
            supports_market_orders: true,
            requires_market_protection: true,
            protection_modes: vec![ProtectionMode::BrokerOco],
            products: vec![ProductType::Delivery],
            order_types: vec![OrderType::Limit, OrderType::StopLimit, OrderType::Market],
            validities: vec![Validity::Day],
        },
    }
}

/// A cash equity: tick 0.05, whole shares, long-only overnight.
pub fn equity_data() -> InstrumentSpecData {
    base(
        "TEST-EQ",
        Venue::Nse,
        AssetClass::Equity,
        InstrumentKind::CashEquity,
        Currency::INR,
    )
}

pub fn equity() -> InstrumentSpec {
    InstrumentSpec::new(equity_data()).unwrap()
}

/// A crude-oil future: tick 1, quantity in lots, multiplier 100 (barrels per lot), shortable.
pub fn crude_future() -> InstrumentSpec {
    let mut data = base(
        "TEST-CRUDE",
        Venue::Mcx,
        AssetClass::Energy,
        InstrumentKind::Future,
        Currency::INR,
    );
    data.tick_size = Decimal::ONE;
    data.multiplier = dec!(100);
    data.expiry = NaiveDate::from_ymd_opt(2026, 4, 17);
    data.calendar_id = CalendarId("mcx".to_owned());
    data.correlation_bucket = CorrelationBucket("energy".to_owned());
    data.capabilities.can_short_overnight = true;
    data.capabilities.products = vec![ProductType::Margin];
    data.capabilities.protection_modes = vec![ProtectionMode::BrokerStopOnly];
    InstrumentSpec::new(data).unwrap()
}

/// A crypto spot pair quoted in USDT with fractional quantities.
pub fn crypto_spot() -> InstrumentSpec {
    let mut data = base(
        "TEST-BTCUSDT",
        Venue::Crypto {
            exchange: "test-venue".to_owned(),
        },
        AssetClass::Crypto,
        InstrumentKind::Spot,
        Currency::USDT,
    );
    data.tick_size = dec!(0.01);
    data.lot_size = dec!(0.00001);
    data.quantity_step = dec!(0.00001);
    data.min_quantity = dec!(0.0001);
    data.calendar_id = CalendarId("crypto_24x7".to_owned());
    data.correlation_bucket = CorrelationBucket("crypto".to_owned());
    data.capabilities.products = vec![ProductType::Spot];
    data.capabilities.protection_modes = vec![ProtectionMode::Unavailable];
    InstrumentSpec::new(data).unwrap()
}
