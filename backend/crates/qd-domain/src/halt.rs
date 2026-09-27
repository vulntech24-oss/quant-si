//! Halts, the kill switch (spec §6.6 "Halt", INV-02, INV-06, INV-07).
//!
//! An entry is allowed only if no active halt covers its account, strategy
//! version or instrument. Risk-reducing orders are never blocked by a halt.
//! An unknown kill-switch state counts as halted.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::action::RiskEffect;
use crate::ids::{AccountId, HaltId, InstrumentId, StrategyVersionId, UserId};

/// Why a halt exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HaltKind {
    /// Set by the owner. Needs a human to re-arm.
    Manual,
    /// Automatic cool-off, for example after consecutive losses.
    CoolOff,
    /// Hard halt, for example on the drawdown limit. Survives restarts; needs a human to re-arm.
    HardHalt,
    /// Operational problem: stale data, broker session lost, reconciliation mismatch.
    Operational,
    /// Set at startup until reconciliation and health checks pass (INV-07).
    Startup,
}

impl HaltKind {
    /// Kinds that can only ever be cleared by a human.
    #[must_use]
    pub const fn always_requires_manual_rearm(self) -> bool {
        matches!(self, Self::Manual | Self::HardHalt)
    }
}

/// What a halt covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "scope", content = "id", rename_all = "snake_case")]
pub enum HaltScope {
    /// Everything.
    Global,
    /// One account.
    Account(AccountId),
    /// One strategy version.
    StrategyVersion(StrategyVersionId),
    /// One instrument.
    Instrument(InstrumentId),
}

/// Who cleared a halt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "by", content = "user", rename_all = "snake_case")]
pub enum ClearedBy {
    /// A human re-armed it.
    Human(UserId),
    /// The system cleared it, for example once startup reconciliation passed.
    System,
}

/// Invalid halt construction or clearing.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HaltError {
    /// Manual and hard halts must require a manual re-arm and cannot auto-clear.
    #[error("{0:?} halts require a manual re-arm and cannot auto-clear")]
    KindRequiresManualRearm(HaltKind),
    /// An auto-clear time must be after the start.
    #[error("auto_clear_at must be after started_at")]
    AutoClearBeforeStart,
    /// The reason is empty.
    #[error("a halt needs a reason")]
    EmptyReason,
    /// The halt was already cleared.
    #[error("halt is already cleared")]
    AlreadyCleared,
    /// Only a human can clear this halt (INV-07).
    #[error("only a human can re-arm this halt")]
    HumanRearmRequired,
    /// A halt cannot be cleared before it started.
    #[error("cannot clear a halt before it started")]
    ClearedBeforeStart,
}

/// One halt. Built only through [`Halt::new`] and [`Halt::clear`], so its rules always hold.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Halt {
    id: HaltId,
    kind: HaltKind,
    scope: HaltScope,
    reason: String,
    started_at: DateTime<Utc>,
    auto_clear_at: Option<DateTime<Utc>>,
    requires_manual_rearm: bool,
    cleared_at: Option<DateTime<Utc>>,
    cleared_by: Option<ClearedBy>,
}

impl Halt {
    /// Creates an active halt.
    ///
    /// Manual and hard halts always require a manual re-arm and never auto-clear.
    pub fn new(
        id: HaltId,
        kind: HaltKind,
        scope: HaltScope,
        reason: impl Into<String>,
        started_at: DateTime<Utc>,
        auto_clear_at: Option<DateTime<Utc>>,
        requires_manual_rearm: bool,
    ) -> Result<Self, HaltError> {
        let reason = reason.into();
        if reason.trim().is_empty() {
            return Err(HaltError::EmptyReason);
        }
        if kind.always_requires_manual_rearm()
            && (!requires_manual_rearm || auto_clear_at.is_some())
        {
            return Err(HaltError::KindRequiresManualRearm(kind));
        }
        if auto_clear_at.is_some_and(|t| t <= started_at) {
            return Err(HaltError::AutoClearBeforeStart);
        }
        Ok(Self {
            id,
            kind,
            scope,
            reason,
            started_at,
            auto_clear_at,
            requires_manual_rearm,
            cleared_at: None,
            cleared_by: None,
        })
    }

    /// Halt id.
    #[must_use]
    pub const fn id(&self) -> HaltId {
        self.id
    }

    /// Kind.
    #[must_use]
    pub const fn kind(&self) -> HaltKind {
        self.kind
    }

    /// Scope.
    #[must_use]
    pub const fn scope(&self) -> HaltScope {
        self.scope
    }

    /// Reason.
    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }

    /// When it started.
    #[must_use]
    pub const fn started_at(&self) -> DateTime<Utc> {
        self.started_at
    }

    /// When it clears on its own, if it does.
    #[must_use]
    pub const fn auto_clear_at(&self) -> Option<DateTime<Utc>> {
        self.auto_clear_at
    }

    /// Whether only a human can clear it.
    #[must_use]
    pub const fn requires_manual_rearm(&self) -> bool {
        self.requires_manual_rearm
    }

    /// When it was cleared.
    #[must_use]
    pub const fn cleared_at(&self) -> Option<DateTime<Utc>> {
        self.cleared_at
    }

    /// Who cleared it.
    #[must_use]
    pub const fn cleared_by(&self) -> Option<ClearedBy> {
        self.cleared_by
    }

    /// Whether the halt is in force at `at`.
    #[must_use]
    pub fn is_active_at(&self, at: DateTime<Utc>) -> bool {
        if at < self.started_at {
            return false;
        }
        if self.cleared_at.is_some_and(|cleared| cleared <= at) {
            return false;
        }
        let auto_cleared =
            !self.requires_manual_rearm && self.auto_clear_at.is_some_and(|t| at >= t);
        !auto_cleared
    }

    /// Whether the halt applies to an order in this context.
    #[must_use]
    pub fn covers(&self, context: &OrderContext) -> bool {
        match self.scope {
            HaltScope::Global => true,
            HaltScope::Account(id) => id == context.account,
            HaltScope::StrategyVersion(id) => context.strategy_version == Some(id),
            HaltScope::Instrument(id) => id == context.instrument,
        }
    }

    /// Clears (re-arms) the halt. Returns the cleared halt; the original is unchanged
    /// so the event history stays append-only.
    pub fn clear(&self, by: ClearedBy, at: DateTime<Utc>) -> Result<Self, HaltError> {
        if self.cleared_at.is_some() {
            return Err(HaltError::AlreadyCleared);
        }
        if self.requires_manual_rearm && by == ClearedBy::System {
            return Err(HaltError::HumanRearmRequired);
        }
        if at < self.started_at {
            return Err(HaltError::ClearedBeforeStart);
        }
        let mut cleared = self.clone();
        cleared.cleared_at = Some(at);
        cleared.cleared_by = Some(by);
        Ok(cleared)
    }
}

/// What an order touches, for halt coverage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OrderContext {
    /// Account.
    pub account: AccountId,
    /// Strategy version, if the order comes from one (manual orders have none).
    pub strategy_version: Option<StrategyVersionId>,
    /// Instrument.
    pub instrument: InstrumentId,
}

/// The kill-switch state as read from storage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HaltState {
    /// The halts are known.
    Known(Vec<Halt>),
    /// The state could not be read or verified. Counts as halted (INV-06).
    Unknown,
}

/// Why halts blocked an order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "block", rename_all = "snake_case")]
pub enum HaltBlock {
    /// An active halt covers the order.
    Active {
        /// The halt.
        halt: HaltId,
        /// Its kind.
        kind: HaltKind,
        /// Its scope.
        scope: HaltScope,
    },
    /// The kill-switch state is unknown.
    StateUnknown,
}

/// Checks an order against the halts at time `at`.
///
/// Risk-reducing orders always pass: a halt blocks risk, never exits (INV-02).
/// They must still pass every other validation. Risk-increasing orders are
/// blocked by any active covering halt, and by an unknown state (INV-06).
pub fn check_order(
    effect: RiskEffect,
    state: &HaltState,
    context: &OrderContext,
    at: DateTime<Utc>,
) -> Result<(), HaltBlock> {
    if effect == RiskEffect::Reducing {
        return Ok(());
    }
    match state {
        HaltState::Unknown => Err(HaltBlock::StateUnknown),
        HaltState::Known(halts) => halts
            .iter()
            .find(|halt| halt.is_active_at(at) && halt.covers(context))
            .map_or(Ok(()), |halt| {
                Err(HaltBlock::Active {
                    halt: halt.id,
                    kind: halt.kind,
                    scope: halt.scope,
                })
            }),
    }
}
