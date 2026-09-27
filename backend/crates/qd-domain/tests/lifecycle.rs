//! Exhaustive transition tables for the §6.6 state machines: every
//! (state, event) pair is either in the legal table or must be an error.

// Test code: a failed unwrap is a failed test. The workspace-wide unwrap/expect
// ban targets runtime code, and clippy's allow-unwrap-in-tests does not reach
// helper functions in integration-test files.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use qd_domain::ids::{EvidenceId, UserId};
use qd_domain::lifecycle::order::{OrderEvent, OrderIntentState, ReconciledState};
use qd_domain::lifecycle::position::{PositionEvent, PositionState};
use qd_domain::lifecycle::strategy::{OwnerApproval, StageEvent, StrategyStage, TradingStage};
use qd_domain::outcome::ExitReason;

// ---------- order intent ----------

fn order_states() -> Vec<OrderIntentState> {
    use OrderIntentState as S;
    vec![
        S::Created,
        S::GatewayRejected,
        S::PendingSubmit,
        S::Submitted,
        S::PartiallyFilled,
        S::Filled,
        S::Cancelled,
        S::BrokerRejected,
        S::Expired,
        S::Unknown,
    ]
}

fn reconciled_states() -> [ReconciledState; 6] {
    use ReconciledState as R;
    [
        R::Submitted,
        R::PartiallyFilled,
        R::Filled,
        R::Cancelled,
        R::BrokerRejected,
        R::Expired,
    ]
}

fn order_events() -> Vec<OrderEvent> {
    use OrderEvent as E;
    let mut events = vec![
        E::GatewayAccepted,
        E::GatewayRejected {
            reason: "limit".to_owned(),
        },
        E::BrokerAcknowledged {
            broker_order_id: "B1".to_owned(),
        },
        E::PartiallyFilled,
        E::Filled,
        E::Cancelled,
        E::BrokerRejected {
            reason: "margin".to_owned(),
        },
        E::Expired,
        E::TransportFailure {
            detail: "timeout".to_owned(),
        },
    ];
    events.extend(reconciled_states().map(|observed| E::Reconciled { observed }));
    events
}

fn legal_order_transition(state: OrderIntentState, event: &OrderEvent) -> Option<OrderIntentState> {
    use OrderEvent as E;
    use OrderIntentState as S;
    let working = matches!(state, S::Submitted | S::PartiallyFilled);
    match event {
        E::GatewayAccepted if state == S::Created => Some(S::PendingSubmit),
        E::GatewayRejected { .. } if state == S::Created => Some(S::GatewayRejected),
        E::BrokerAcknowledged { .. } if state == S::PendingSubmit => Some(S::Submitted),
        E::BrokerRejected { .. } if matches!(state, S::PendingSubmit | S::Submitted) => {
            Some(S::BrokerRejected)
        }
        E::PartiallyFilled if working => Some(S::PartiallyFilled),
        E::Filled if working => Some(S::Filled),
        E::Cancelled if working => Some(S::Cancelled),
        E::Expired if working => Some(S::Expired),
        E::TransportFailure { .. }
            if matches!(state, S::PendingSubmit | S::Submitted | S::PartiallyFilled) =>
        {
            Some(S::Unknown)
        }
        E::Reconciled { observed } if state == S::Unknown => Some((*observed).into()),
        _ => None,
    }
}

#[test]
fn order_intent_transition_table_is_exact() {
    for state in order_states() {
        for event in order_events() {
            let result = state.apply(&event);
            match legal_order_transition(state, &event) {
                Some(next) => assert_eq!(result, Ok(next), "{state:?} + {event:?}"),
                None => assert!(result.is_err(), "{state:?} + {event:?} must be illegal"),
            }
        }
    }
}

#[test]
fn terminal_order_states_accept_no_events() {
    for state in order_states().into_iter().filter(|s| s.is_terminal()) {
        for event in order_events() {
            assert!(state.apply(&event).is_err(), "{state:?} is terminal");
        }
    }
}

#[test]
fn unknown_orders_resolve_only_through_reconciliation() {
    let unknown = OrderIntentState::Unknown;
    for event in order_events() {
        let is_reconcile = matches!(event, OrderEvent::Reconciled { .. });
        assert_eq!(unknown.apply(&event).is_ok(), is_reconcile, "{event:?}");
    }
}

// ---------- position ----------

fn position_states() -> [PositionState; 6] {
    use PositionState as S;
    [
        S::Opening,
        S::Open,
        S::Protected,
        S::Unprotected,
        S::Exiting,
        S::Closed,
    ]
}

fn position_events() -> Vec<PositionEvent> {
    use PositionEvent as E;
    vec![
        E::EntryFilled,
        E::EntryAbandoned,
        E::ProtectionConfirmed,
        E::ProtectionLost,
        E::ExitStarted {
            reason: ExitReason::TargetHit,
        },
        E::ExitFailed,
        E::ExitCompleted,
    ]
}

fn legal_position_transition(state: PositionState, event: &PositionEvent) -> Option<PositionState> {
    use PositionEvent as E;
    use PositionState as S;
    match (state, event) {
        (S::Opening, E::EntryFilled) => Some(S::Open),
        (S::Opening, E::EntryAbandoned) => Some(S::Closed),
        (S::Open | S::Unprotected, E::ProtectionConfirmed) => Some(S::Protected),
        (S::Open | S::Protected, E::ProtectionLost) => Some(S::Unprotected),
        (S::Open | S::Protected | S::Unprotected, E::ExitStarted { .. }) => Some(S::Exiting),
        (S::Exiting, E::ExitFailed) => Some(S::Unprotected),
        (S::Exiting, E::ExitCompleted) => Some(S::Closed),
        _ => None,
    }
}

#[test]
fn position_transition_table_is_exact() {
    for state in position_states() {
        for event in position_events() {
            let result = state.apply(&event);
            match legal_position_transition(state, &event) {
                Some(next) => assert_eq!(result, Ok(next), "{state:?} + {event:?}"),
                None => assert!(result.is_err(), "{state:?} + {event:?} must be illegal"),
            }
        }
    }
}

// ---------- strategy stage ----------

fn stages() -> Vec<StrategyStage> {
    use StrategyStage as S;
    let mut stages = vec![
        S::Draft,
        S::Research,
        S::Rejected,
        S::ResearchPassed,
        S::Paper,
        S::SmallCapital,
        S::Full,
        S::Retired,
    ];
    stages.extend(
        [
            TradingStage::Paper,
            TradingStage::SmallCapital,
            TradingStage::Full,
        ]
        .map(|resume_to| S::Suspended { resume_to }),
    );
    stages
}

fn stage_events() -> Vec<StageEvent> {
    use StageEvent as E;
    let owner = UserId::new_at(common::at(0, 0));
    let evidence = EvidenceId::new_at(common::at(0, 0));
    let approval = OwnerApproval {
        approved_by: owner,
        approved_at: common::at(12, 0),
        evidence,
    };
    let mut events = vec![
        E::StartResearch,
        E::RejectResearch {
            reason: "failed holdout".to_owned(),
        },
        E::PassResearch { evidence },
        E::Suspend {
            by: owner,
            reason: "review".to_owned(),
        },
        E::Resume { by: owner },
        E::AutoDemote {
            reason: "breach".to_owned(),
        },
        E::Retire {
            reason: "superseded".to_owned(),
        },
    ];
    events.extend(
        [
            TradingStage::Paper,
            TradingStage::SmallCapital,
            TradingStage::Full,
        ]
        .map(|to| E::Promote { to, approval }),
    );
    events
}

fn legal_stage_transition(stage: StrategyStage, event: &StageEvent) -> Option<StrategyStage> {
    use StageEvent as E;
    use StrategyStage as S;
    use TradingStage as T;
    match (stage, event) {
        (S::Retired, _) => None,
        (_, E::Retire { .. }) => Some(S::Retired),
        (S::Draft, E::StartResearch) => Some(S::Research),
        (S::Research, E::RejectResearch { .. }) => Some(S::Rejected),
        (S::Research, E::PassResearch { .. }) => Some(S::ResearchPassed),
        (S::ResearchPassed, E::Promote { to: T::Paper, .. }) => Some(S::Paper),
        (
            S::Paper,
            E::Promote {
                to: T::SmallCapital,
                ..
            },
        ) => Some(S::SmallCapital),
        (S::SmallCapital, E::Promote { to: T::Full, .. }) => Some(S::Full),
        (S::Paper, E::Suspend { .. }) => Some(S::Suspended {
            resume_to: T::Paper,
        }),
        (S::SmallCapital, E::Suspend { .. }) => Some(S::Suspended {
            resume_to: T::SmallCapital,
        }),
        (S::Full, E::Suspend { .. }) => Some(S::Suspended { resume_to: T::Full }),
        (S::Suspended { resume_to }, E::Resume { .. }) => Some(resume_to.into()),
        (S::SmallCapital | S::Full, E::AutoDemote { .. }) => Some(S::Paper),
        _ => None,
    }
}

#[test]
fn strategy_stage_transition_table_is_exact() {
    for stage in stages() {
        for event in stage_events() {
            let result = stage.apply(&event);
            match legal_stage_transition(stage, &event) {
                Some(next) => assert_eq!(result, Ok(next), "{stage:?} + {event:?}"),
                None => assert!(result.is_err(), "{stage:?} + {event:?} must be illegal"),
            }
        }
    }
}

#[test]
fn transition_errors_name_the_machine_state_and_event() {
    let err = OrderIntentState::Filled
        .apply(&OrderEvent::Cancelled)
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "illegal order intent transition: cancelled from Filled"
    );
}

#[test]
fn events_serialize_for_the_append_only_log() {
    let event = StageEvent::AutoDemote {
        reason: "breach".to_owned(),
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["event"], "auto_demote");
    assert_eq!(serde_json::from_value::<StageEvent>(json).unwrap(), event);
}
