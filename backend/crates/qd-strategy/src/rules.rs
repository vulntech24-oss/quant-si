//! Small helpers shared by the strategies: explanations in percent, and the
//! reason direction.

use qd_domain::proposal::{FactorValue, Reason, ReasonDirection};
use rust_decimal::Decimal;

/// A fraction as a percentage rounded to two places.
pub(crate) fn pct(value: Decimal) -> Option<Decimal> {
    value
        .checked_mul(Decimal::ONE_HUNDRED)
        .map(|v| v.round_dp(2))
}

/// A numeric reason.
pub(crate) fn reason(factor: &str, value: Decimal, supports: bool) -> Reason {
    Reason {
        factor: factor.to_owned(),
        value: FactorValue::Number(value),
        direction: if supports {
            ReasonDirection::Supports
        } else {
            ReasonDirection::Opposes
        },
    }
}
