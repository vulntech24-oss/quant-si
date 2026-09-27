//! Cost model: the shipped schedules parse and validate, and hand-computed
//! contract-note examples match.

// Test code: a failed unwrap is a failed test. The workspace-wide unwrap/expect
// ban targets runtime code, and clippy's allow-unwrap-in-tests does not reach
// helper functions in integration-test files.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use qd_domain::action::Side;
use qd_domain::costs::{
    CostError, CostModel, CostRequest, CostScheduleSet, ScheduleCostModel, VerificationStatus,
};
use qd_domain::instrument::{AssetClass, InstrumentKind, InstrumentSpec, ProductType};
use qd_domain::num::{Price, Quantity};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

const SHIPPED: &str = include_str!("../../../config/costs/india-zerodha.toml");

fn shipped() -> ScheduleCostModel {
    let set: CostScheduleSet = toml::from_str(SHIPPED).unwrap();
    ScheduleCostModel::new(set).unwrap()
}

fn line(quote: &qd_domain::costs::CostQuote, name: &str) -> Option<Decimal> {
    quote
        .estimate
        .lines()
        .iter()
        .find(|l| l.name == name)
        .map(|l| l.amount)
}

fn request<'a>(
    spec: &'a InstrumentSpec,
    product: ProductType,
    side: Side,
    quantity: Decimal,
    entry: Decimal,
    exit: Decimal,
) -> CostRequest<'a> {
    CostRequest {
        spec,
        product,
        side,
        quantity: Quantity::new(quantity).unwrap(),
        entry: Price::new(entry).unwrap(),
        exit: Price::new(exit).unwrap(),
        trade_date: common::date(),
    }
}

#[test]
fn shipped_schedules_parse_validate_and_record_their_verification_source() {
    let model = shipped();
    assert_eq!(model.schedules().len(), 3);
    for schedule in model.schedules() {
        let v = &schedule.data().verification;
        assert_eq!(
            v.status,
            VerificationStatus::Verified,
            "{}",
            schedule.data().id
        );
        assert!(schedule.is_verified());
        assert!(
            v.sources.iter().any(|s| s.contains("zerodha.com/charges")),
            "{} must cite the official page",
            schedule.data().id
        );
    }
}

#[test]
fn equity_delivery_round_trip_worked_example() {
    // 100 shares, entry 1,000, exit 1,100.
    // Purchase leg, turnover 100,000: STT 100.00, exchange 3.07, SEBI 0.10,
    //   stamp 15.00, GST 0.18 × 3.17 = 0.57.
    // Disposal leg, turnover 110,000: STT 110.00, exchange 3.38, SEBI 0.11,
    //   DP 13.00, GST 0.18 × 16.49 = 2.97.
    let spec = common::equity();
    let quote = shipped()
        .quote(&request(
            &spec,
            ProductType::Delivery,
            Side::Long,
            dec!(100),
            dec!(1000),
            dec!(1100),
        ))
        .unwrap();
    assert_eq!(
        quote.estimate.model_version(),
        "zerodha-nse-equity-delivery@1"
    );
    assert!(quote.verified);
    assert_eq!(line(&quote, "brokerage"), Some(dec!(0)));
    assert_eq!(line(&quote, "stt"), Some(dec!(210.00)));
    assert_eq!(line(&quote, "exchange_transaction"), Some(dec!(6.45)));
    assert_eq!(line(&quote, "sebi_fee"), Some(dec!(0.21)));
    assert_eq!(line(&quote, "stamp_duty"), Some(dec!(15.00)));
    assert_eq!(line(&quote, "dp_charge"), Some(dec!(13)));
    assert_eq!(line(&quote, "gst"), Some(dec!(3.54)));
    assert_eq!(quote.estimate.total(), dec!(248.20));
    assert_eq!(quote.estimate.per_unit(), dec!(2.482));
}

#[test]
fn mcx_short_round_trip_uses_the_multiplier_and_the_brokerage_cap() {
    // One crude lot (100 barrels) sold short at 6,000 and covered at 5,760.
    // Disposal leg, turnover 600,000: brokerage min(180, 20) = 20, CTT 60.00,
    //   exchange 12.60, SEBI 0.60, GST 0.18 × 33.20 = 5.98.
    // Purchase leg, turnover 576,000: brokerage 20, exchange 12.10, SEBI 0.58,
    //   stamp 11.52, GST 0.18 × 32.68 = 5.88.
    let spec = common::crude_future();
    let quote = shipped()
        .quote(&request(
            &spec,
            ProductType::Margin,
            Side::Short,
            dec!(1),
            dec!(6000),
            dec!(5760),
        ))
        .unwrap();
    assert_eq!(line(&quote, "brokerage"), Some(dec!(40)));
    assert_eq!(line(&quote, "ctt"), Some(dec!(60.00)));
    assert_eq!(line(&quote, "exchange_transaction"), Some(dec!(24.70)));
    assert_eq!(line(&quote, "sebi_fee"), Some(dec!(1.18)));
    assert_eq!(line(&quote, "stamp_duty"), Some(dec!(11.52)));
    assert_eq!(line(&quote, "gst"), Some(dec!(11.86)));
    assert_eq!(quote.estimate.total(), dec!(149.26));
}

#[test]
fn gold_etfs_use_the_commodity_etf_schedule() {
    let mut data = common::equity_data();
    data.kind = InstrumentKind::Etf;
    data.asset_class = AssetClass::PreciousMetal;
    let spec = InstrumentSpec::new(data).unwrap();
    let quote = shipped()
        .quote(&request(
            &spec,
            ProductType::Delivery,
            Side::Long,
            dec!(10),
            dec!(80),
            dec!(90),
        ))
        .unwrap();
    assert_eq!(
        quote.estimate.model_version(),
        "zerodha-nse-commodity-etf-delivery@1"
    );
    assert_eq!(line(&quote, "stt"), None);
}

#[test]
fn uncovered_requests_fail_closed() {
    let model = shipped();
    let crypto = common::crypto_spot();
    assert!(matches!(
        model.quote(&request(
            &crypto,
            ProductType::Spot,
            Side::Long,
            dec!(0.01),
            dec!(60000),
            dec!(66000)
        )),
        Err(CostError::NoSchedule { .. })
    ));
    let equity = common::equity();
    // Wrong product for the instrument.
    assert!(matches!(
        model.quote(&request(
            &equity,
            ProductType::Intraday,
            Side::Long,
            dec!(1),
            dec!(100),
            dec!(110)
        )),
        Err(CostError::NoSchedule { .. })
    ));
    assert_eq!(
        model.quote(&request(
            &equity,
            ProductType::Delivery,
            Side::Long,
            dec!(0),
            dec!(100),
            dec!(110)
        )),
        Err(CostError::ZeroQuantity)
    );
}

#[test]
fn invalid_or_overlapping_schedules_are_rejected() {
    let mut set: CostScheduleSet = toml::from_str(SHIPPED).unwrap();
    let mut broken = set.clone();
    broken.schedules[0].gst_rate = dec!(1.5);
    assert!(matches!(
        ScheduleCostModel::new(broken),
        Err(CostError::InvalidSchedule { .. })
    ));

    let mut duplicate = set.clone();
    duplicate.schedules.push(duplicate.schedules[0].clone());
    assert!(ScheduleCostModel::new(duplicate).is_err());

    // Two versions covering the same date make quotes ambiguous: fail closed.
    let mut v2 = set.schedules[0].clone();
    v2.version = 2;
    set.schedules.push(v2);
    let model = ScheduleCostModel::new(set).unwrap();
    let spec = common::equity();
    assert!(matches!(
        model.quote(&request(
            &spec,
            ProductType::Delivery,
            Side::Long,
            dec!(1),
            dec!(100),
            dec!(110)
        )),
        Err(CostError::AmbiguousSchedule { count: 2, .. })
    ));
}
