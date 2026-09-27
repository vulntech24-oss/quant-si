//! Pure order-validation rules used by the Order Gateway in every mode (INV-08).
//!
//! Halt checks live in [`crate::halt::check_order`].

use serde::Serialize;
use thiserror::Error;

use crate::num::Quantity;

/// Why an exit quantity is invalid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error, Serialize)]
#[serde(tag = "error", rename_all = "snake_case")]
pub enum ExitQuantityError {
    /// Nothing to exit.
    #[error("exit quantity must be greater than zero")]
    Zero,
    /// The exit is larger than the open position and would reverse it.
    #[error("exit quantity {exit} exceeds open quantity {open}")]
    ExceedsOpen {
        /// Open quantity.
        open: Quantity,
        /// Requested exit quantity.
        exit: Quantity,
    },
}

/// An exit can never exceed the open quantity, so it can never reverse a position (INV-02).
pub fn check_exit_quantity(open: Quantity, exit: Quantity) -> Result<(), ExitQuantityError> {
    if exit.is_zero() {
        return Err(ExitQuantityError::Zero);
    }
    if exit > open {
        return Err(ExitQuantityError::ExceedsOpen { open, exit });
    }
    Ok(())
}
