//! Halts the account state calls for (spec §4 loss limits, §6.6 "Halt", INV-07).
//!
//! - Drawdown at or beyond the hard-halt limit → a `HardHalt` on the account.
//!   It survives restarts and only a human can re-arm it.
//! - Consecutive losses at or beyond the threshold → a `CoolOff` on the account
//!   that clears automatically after the configured duration.
//!
//! A trigger is not proposed again while an active halt of the same kind
//! already covers the account.

use chrono::{DateTime, Duration, Utc};
use qd_domain::halt::{Halt, HaltError, HaltKind, HaltScope, HaltState};
use qd_domain::ids::HaltId;
use qd_domain::portfolio::{PortfolioError, drawdown};
use serde::Serialize;

use crate::config::RiskConfig;
use crate::gate::AccountRiskState;

/// A halt the Risk Gate asks the kill switch to record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HaltProposal {
    /// Kind.
    pub kind: HaltKind,
    /// Scope.
    pub scope: HaltScope,
    /// Reason.
    pub reason: String,
    /// When it clears by itself, if it does.
    pub auto_clear_at: Option<DateTime<Utc>>,
    /// Whether only a human can clear it.
    pub requires_manual_rearm: bool,
}

impl HaltProposal {
    /// Turns the proposal into a halt starting at `started_at`.
    pub fn into_halt(self, id: HaltId, started_at: DateTime<Utc>) -> Result<Halt, HaltError> {
        Halt::new(
            id,
            self.kind,
            self.scope,
            self.reason,
            started_at,
            self.auto_clear_at,
            self.requires_manual_rearm,
        )
    }
}

fn already_halted(state: &AccountRiskState, kind: HaltKind, at: DateTime<Utc>) -> bool {
    match &state.halts {
        // With the state unknown we cannot tell; propose the halt anyway. A
        // duplicate record is harmless, a missing hard halt is not.
        HaltState::Unknown => false,
        HaltState::Known(halts) => halts.iter().any(|halt| {
            halt.kind() == kind
                && halt.is_active_at(at)
                && (halt.scope() == HaltScope::Global
                    || halt.scope() == HaltScope::Account(state.account))
        }),
    }
}

/// Halts the account state calls for at `at`.
///
/// Fails if the equity figures are inconsistent; the caller should then treat
/// the account as operationally halted (INV-06).
pub fn halt_triggers(
    state: &AccountRiskState,
    config: &RiskConfig,
    at: DateTime<Utc>,
) -> Result<Vec<HaltProposal>, PortfolioError> {
    let mut proposals = Vec::new();
    let dd = drawdown(state.high_water_mark, state.equity)?;
    if dd >= config.hard_halt_drawdown && !already_halted(state, HaltKind::HardHalt, at) {
        proposals.push(HaltProposal {
            kind: HaltKind::HardHalt,
            scope: HaltScope::Account(state.account),
            reason: format!(
                "drawdown {} reached the hard-halt limit {}",
                dd.value(),
                config.hard_halt_drawdown.value()
            ),
            auto_clear_at: None,
            requires_manual_rearm: true,
        });
    }
    if state.consecutive_losses >= config.cool_off_after_losses
        && !already_halted(state, HaltKind::CoolOff, at)
    {
        proposals.push(HaltProposal {
            kind: HaltKind::CoolOff,
            scope: HaltScope::Account(state.account),
            reason: format!(
                "{} losing trades in a row (cool-off after {})",
                state.consecutive_losses, config.cool_off_after_losses
            ),
            auto_clear_at: Some(at + Duration::hours(i64::from(config.cool_off_hours))),
            requires_manual_rearm: false,
        });
    }
    Ok(proposals)
}
