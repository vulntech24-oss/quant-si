//! Decision Engine, Order Gateway and Position Manager, through the real pipeline.

// Test code: a failed unwrap is a failed test. The workspace-wide unwrap/expect
// ban targets runtime code, and clippy's allow-unwrap-in-tests does not reach
// helper functions in integration-test files.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::sync::atomic::Ordering;

use chrono::Duration;
use qd_app::decision::EvidencePolicy;
use qd_app::gateway::GatewayRejection;
use qd_app::journal::JournalEntry;
use qd_app::live::{Environment, LiveBlock, LivePolicy, check_live_order, live_orders_compiled};
use qd_app::memory::InMemoryJournal;
use qd_app::ports::{BrokerFill, HaltStore};
use qd_app::positions::PositionManager;
use qd_domain::action::RiskEffect;
use qd_domain::halt::{Halt, HaltBlock, HaltKind, HaltScope};
use qd_domain::ids::HaltId;
use qd_domain::instrument::ProductType;
use qd_domain::lifecycle::order::{OrderIntentState, ReconciledState};
use qd_domain::lifecycle::position::PositionState;
use qd_domain::lifecycle::strategy::StrategyStage;
use qd_domain::num::{Price, Quantity};
use qd_domain::outcome::{DecisionOutcome, ExitReason, NoTradeReason};
use qd_domain::proposal::AccountMode;
use rust_decimal_macros::dec;

use common::*;

const EVIDENCE: EvidencePolicy = EvidencePolicy::Required { min_evidence: 30 };

async fn approved(mode: AccountMode) -> qd_app::decision::JournaledDecision {
    let journal = InMemoryJournal::new();
    decide(
        mode,
        StrategyStage::Paper,
        &FakeEvidence(Some(40)),
        EVIDENCE,
        &journal,
    )
    .await
    .unwrap()
    .unwrap()
}

// ---------- Decision Engine ----------

#[tokio::test]
async fn an_approved_setup_is_journaled_before_it_is_authorized() {
    let journal = InMemoryJournal::new();
    let decided = decide(
        AccountMode::Paper,
        StrategyStage::Paper,
        &FakeEvidence(Some(40)),
        EVIDENCE,
        &journal,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(
        decided.record.outcome,
        DecisionOutcome::Enter { .. }
    ));
    let auth = decided.authorization.unwrap();
    assert_eq!(
        auth.quantity(),
        decided.record.approval.as_ref().unwrap().quantity
    );
    let entries = journal.entries();
    assert_eq!(entries.len(), 1);
    assert!(matches!(&entries[0], JournalEntry::Decision(r) if r.id == decided.record.id));
}

#[tokio::test]
async fn missing_or_thin_evidence_is_no_trade() {
    let journal = InMemoryJournal::new();
    for (evidence, n) in [(FakeEvidence(None), 0), (FakeEvidence(Some(12)), 12)] {
        let decided = decide(
            AccountMode::Paper,
            StrategyStage::Paper,
            &evidence,
            EVIDENCE,
            &journal,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            decided.record.outcome,
            DecisionOutcome::NoTrade {
                reason: NoTradeReason::InsufficientEvidence { n, min: 30 }
            }
        );
        assert!(decided.authorization.is_none());
    }
}

#[tokio::test]
async fn research_priors_are_refused_outside_backtests() {
    let journal = InMemoryJournal::new();
    let decided = decide(
        AccountMode::Paper,
        StrategyStage::Paper,
        &FakeEvidence(None),
        EvidencePolicy::ResearchPrior,
        &journal,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(
        decided.record.outcome,
        DecisionOutcome::NoTrade {
            reason: NoTradeReason::MissingOrInconsistentData { .. }
        }
    ));
}

#[tokio::test]
async fn invariant_03_a_risk_rejection_releases_no_authorization() {
    let journal = InMemoryJournal::new();
    let decided = decide(
        AccountMode::Paper,
        StrategyStage::Draft,
        &FakeEvidence(Some(40)),
        EVIDENCE,
        &journal,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(
        decided.record.outcome,
        DecisionOutcome::NoTrade {
            reason: NoTradeReason::RiskLimit(_)
        }
    ));
    assert!(decided.authorization.is_none());
    // The NO TRADE decision is journaled like any other (spec §2).
    assert_eq!(journal.entries().len(), 1);
}

#[tokio::test]
async fn invariant_05_no_journal_no_authorization() {
    let journal = FakeJournal::default();
    journal.fail.store(true, Ordering::SeqCst);
    let result = decide(
        AccountMode::Paper,
        StrategyStage::Paper,
        &FakeEvidence(Some(40)),
        EVIDENCE,
        &journal,
    )
    .await;
    assert!(result.is_err());
}

// ---------- Order Gateway ----------

async fn submit_entry(
    h: &Harness,
    decided: &qd_app::decision::JournaledDecision,
) -> Result<qd_domain::ids::PositionId, GatewayRejection> {
    let pm = PositionManager::new(h.gateway.clone(), h.journal.clone(), h.clock.clone());
    pm.open(
        decided.authorization.as_ref().unwrap(),
        decided.record.proposal.as_ref().unwrap(),
        &spec(),
        StrategyStage::Paper,
        ProductType::Delivery,
    )
    .await
}

#[tokio::test]
async fn invariant_05_the_intent_is_journaled_before_the_broker_call() {
    let h = harness(AccountMode::Paper, LivePolicy::default(), false);
    let decided = approved(AccountMode::Paper).await;
    submit_entry(&h, &decided).await.unwrap();
    assert_eq!(h.executor.submits(), 1);
    let entries = h.journal.inner.entries();
    let intent_at = entries
        .iter()
        .position(|e| matches!(e, JournalEntry::OrderIntent { .. }))
        .unwrap();
    let accepted_at = entries
        .iter()
        .position(|e| {
            matches!(
                e,
                JournalEntry::OrderEvent {
                    state: OrderIntentState::PendingSubmit,
                    ..
                }
            )
        })
        .unwrap();
    let acked_at = entries
        .iter()
        .position(|e| {
            matches!(
                e,
                JournalEntry::OrderEvent {
                    state: OrderIntentState::Submitted,
                    ..
                }
            )
        })
        .unwrap();
    assert!(intent_at < accepted_at && accepted_at < acked_at);
}

#[tokio::test]
async fn invariant_05_a_journal_failure_sends_nothing_and_halts_entries() {
    let h = harness(AccountMode::Paper, LivePolicy::default(), false);
    let decided = approved(AccountMode::Paper).await;
    h.journal.fail.store(true, Ordering::SeqCst);
    assert_eq!(
        submit_entry(&h, &decided).await,
        Err(GatewayRejection::JournalUnavailable)
    );
    assert_eq!(h.executor.submits(), 0);
    assert!(h.gateway.entries_halted_by_journal_failure());
    // Even with the journal back, entries stay halted until someone intervenes.
    h.journal.fail.store(false, Ordering::SeqCst);
    let again = approved(AccountMode::Paper).await;
    assert_eq!(
        submit_entry(&h, &again).await,
        Err(GatewayRejection::JournalUnavailable)
    );
    assert_eq!(h.executor.submits(), 0);
}

#[tokio::test]
async fn invariant_02_an_active_halt_blocks_the_entry_at_the_gateway() {
    let h = harness(AccountMode::Paper, LivePolicy::default(), false);
    let decided = approved(AccountMode::Paper).await;
    let now = h.clock.0.lock().unwrap().to_owned();
    let halt = Halt::new(
        HaltId::new_at(now),
        HaltKind::Manual,
        HaltScope::Global,
        "owner pause",
        now - Duration::minutes(1),
        None,
        true,
    )
    .unwrap();
    h.halts.record(&halt).await.unwrap();
    assert!(matches!(
        submit_entry(&h, &decided).await,
        Err(GatewayRejection::Halted(HaltBlock::Active { .. }))
    ));
    assert_eq!(h.executor.submits(), 0);
}

#[tokio::test]
async fn invariant_06_an_unreadable_halt_store_blocks_entries() {
    let executor = Arc::new(FakeExecutor::default());
    let journal = Arc::new(FakeJournal::default());
    let clock = FakeClock::at(at_close(decision_date()));
    let gateway = Arc::new(qd_app::gateway::OrderGateway::new(
        qd_app::gateway::GatewayAccount {
            id: account_id(),
            mode: AccountMode::Paper,
            live_armed: false,
        },
        executor.clone(),
        journal.clone(),
        Arc::new(FakeBrokenHaltStore),
        clock.clone(),
        LivePolicy::default(),
        vec![spec()],
    ));
    let h = Harness {
        executor,
        journal,
        halts: Arc::new(qd_app::memory::InMemoryHaltStore::new()),
        clock,
        gateway,
    };
    let decided = approved(AccountMode::Paper).await;
    assert_eq!(
        submit_entry(&h, &decided).await,
        Err(GatewayRejection::Halted(HaltBlock::StateUnknown))
    );
    assert_eq!(h.executor.submits(), 0);
}

/// Opens a position and fills its entry; returns the manager and position id.
async fn filled_position(h: &Harness) -> (PositionManager, qd_domain::ids::PositionId) {
    let pm = PositionManager::new(h.gateway.clone(), h.journal.clone(), h.clock.clone());
    let decided = approved(AccountMode::Paper).await;
    let id = pm
        .open(
            decided.authorization.as_ref().unwrap(),
            decided.record.proposal.as_ref().unwrap(),
            &spec(),
            StrategyStage::Paper,
            ProductType::Delivery,
        )
        .await
        .unwrap();
    let entry = h.executor.submitted.lock().unwrap()[0].clone();
    let report = h
        .gateway
        .on_fill(BrokerFill {
            client_order_id: entry.client_order_id(),
            quantity: entry.quantity(),
            price: entry.terms().trigger.unwrap(),
            at: at_close(decision_date()),
        })
        .await
        .unwrap();
    let update = pm.on_fill(&report, decision_date()).await;
    assert!(update.unprotected.is_empty(), "{update:?}");
    (pm, id)
}

#[tokio::test]
async fn filled_entries_get_a_stop_and_target_as_one_oco_group() {
    let h = harness(AccountMode::Paper, LivePolicy::default(), false);
    let (pm, id) = filled_position(&h).await;
    let position = pm.positions().into_iter().find(|p| p.id == id).unwrap();
    assert_eq!(position.state, PositionState::Protected);
    let submitted = h.executor.submitted.lock().unwrap().clone();
    assert_eq!(submitted.len(), 3);
    assert_eq!(submitted[1].oco_group(), Some(id));
    assert_eq!(submitted[2].oco_group(), Some(id));
    assert_eq!(submitted[1].quantity(), position.quantity);
    // Stop and target reach the broker in one OCO call.
    assert_eq!(*h.executor.oco_calls.lock().unwrap(), vec![2]);
}

fn protective_leg(
    position: &qd_app::positions::Position,
    group: Option<qd_domain::ids::PositionId>,
    at: chrono::DateTime<chrono::Utc>,
) -> qd_app::orders::OrderIntent {
    qd_app::orders::OrderIntent::reducing(
        qd_domain::ids::OrderIntentId::new_at(at),
        position.account,
        position.instrument,
        position.strategy_version,
        position.id,
        position.side.exit(),
        position.quantity,
        qd_app::orders::OrderTerms {
            order_type: qd_domain::instrument::OrderType::StopMarket,
            limit: None,
            trigger: Some(position.stop),
            validity: qd_domain::instrument::Validity::GoodTillCancelled,
            product: ProductType::Delivery,
        },
        qd_app::orders::OrderPurpose::ProtectiveStop,
        group,
        at,
    )
}

#[tokio::test]
async fn an_oco_group_is_sent_whole_or_not_at_all() {
    let h = harness(AccountMode::Paper, LivePolicy::default(), false);
    let (pm, id) = filled_position(&h).await;
    let position = pm.positions().into_iter().find(|p| p.id == id).unwrap();
    let now = at_close(decision_date());
    let sent = h.executor.submits();

    // A lone leg or legs of different groups are not an OCO group.
    let lone = vec![protective_leg(&position, Some(id), now)];
    assert!(matches!(
        h.gateway.submit_oco(lone, None).await,
        Err(GatewayRejection::InvalidTerms(_))
    ));
    let mixed = vec![
        protective_leg(&position, Some(id), now),
        protective_leg(&position, None, now),
    ];
    assert!(matches!(
        h.gateway.submit_oco(mixed, None).await,
        Err(GatewayRejection::InvalidTerms(_))
    ));

    // The position's own stop and target already cover its quantity, so a
    // second group is refused and none of its legs is sent.
    let other = qd_domain::ids::PositionId::new_at(now);
    let legs = vec![
        protective_leg(&position, Some(other), now),
        protective_leg(&position, Some(other), now),
    ];
    let ids: Vec<_> = legs.iter().map(qd_app::orders::OrderIntent::id).collect();
    assert!(matches!(
        h.gateway.submit_oco(legs, None).await,
        Err(GatewayRejection::ExitExceedsOpen(_))
    ));
    assert_eq!(h.executor.submits(), sent);
    for id in ids {
        assert_eq!(
            h.gateway.intent_state(id),
            Some(OrderIntentState::GatewayRejected)
        );
    }
}

#[tokio::test]
async fn invariant_02_exits_pass_halts_but_never_exceed_the_open_quantity() {
    let h = harness(AccountMode::Paper, LivePolicy::default(), false);
    let (pm, id) = filled_position(&h).await;
    // A global hard halt arrives.
    let now = at_close(decision_date());
    let halt = Halt::new(
        HaltId::new_at(now),
        HaltKind::HardHalt,
        HaltScope::Global,
        "drawdown",
        now,
        None,
        true,
    )
    .unwrap();
    h.halts.record(&halt).await.unwrap();
    let mut update = qd_app::positions::PositionUpdate::default();
    pm.exit(id, ExitReason::Flatten, &mut update).await;
    assert!(update.rejections.is_empty(), "{update:?}");
    // Protection was cancelled, then one market exit for the open quantity was sent.
    assert_eq!(h.executor.cancelled.lock().unwrap().len(), 2);
    let exit = h
        .executor
        .submitted
        .lock()
        .unwrap()
        .last()
        .cloned()
        .unwrap();
    assert_eq!(
        exit.terms().order_type,
        qd_domain::instrument::OrderType::Market
    );
    // With the market exit working, any further exit would exceed the open quantity.
    let position = pm.positions().into_iter().find(|p| p.id == id).unwrap();
    let extra = qd_app::orders::OrderIntent::reducing(
        qd_domain::ids::OrderIntentId::new_at(now),
        position.account,
        position.instrument,
        position.strategy_version,
        id,
        position.side.exit(),
        Quantity::new(dec!(1)).unwrap(),
        qd_app::orders::OrderTerms {
            order_type: qd_domain::instrument::OrderType::Market,
            limit: None,
            trigger: None,
            validity: qd_domain::instrument::Validity::Day,
            product: ProductType::Delivery,
        },
        qd_app::orders::OrderPurpose::Exit {
            reason: ExitReason::Manual,
        },
        None,
        now,
    );
    assert!(matches!(
        h.gateway.submit(extra, None).await,
        Err(GatewayRejection::ExitExceedsOpen(_))
    ));
}

#[tokio::test]
async fn an_oco_fill_cancels_its_sibling_and_closes_the_position() {
    let h = harness(AccountMode::Paper, LivePolicy::default(), false);
    let (pm, id) = filled_position(&h).await;
    let stop = h.executor.submitted.lock().unwrap()[1].clone();
    let report = h
        .gateway
        .on_fill(BrokerFill {
            client_order_id: stop.client_order_id(),
            quantity: stop.quantity(),
            price: stop.terms().trigger.unwrap(),
            at: at_close(decision_date()),
        })
        .await
        .unwrap();
    assert_eq!(report.cancelled_siblings.len(), 1);
    let update = pm.on_fill(&report, decision_date()).await;
    assert_eq!(update.closed, vec![id]);
    let position = pm.positions().into_iter().find(|p| p.id == id).unwrap();
    assert_eq!(position.state, PositionState::Closed);
    assert_eq!(position.exit_reason, Some(ExitReason::StopHit));
    assert!(position.realized_gross < dec!(0));
    assert_eq!(
        h.gateway
            .open_quantity(spec().id, qd_domain::action::Side::Long),
        Quantity::ZERO
    );
}

#[tokio::test]
async fn resubmitting_an_intent_never_calls_the_broker_twice() {
    let h = harness(AccountMode::Paper, LivePolicy::default(), false);
    let decided = approved(AccountMode::Paper).await;
    let pm = PositionManager::new(h.gateway.clone(), h.journal.clone(), h.clock.clone());
    pm.open(
        decided.authorization.as_ref().unwrap(),
        decided.record.proposal.as_ref().unwrap(),
        &spec(),
        StrategyStage::Paper,
        ProductType::Delivery,
    )
    .await
    .unwrap();
    let intent = {
        let entries = h.journal.inner.entries();
        entries
            .iter()
            .find_map(|e| match e {
                JournalEntry::OrderIntent { intent } => Some((**intent).clone()),
                _ => None,
            })
            .unwrap()
    };
    let ack = h
        .gateway
        .submit(intent, Some(StrategyStage::Paper))
        .await
        .unwrap();
    assert_eq!(ack.state, OrderIntentState::Submitted);
    assert_eq!(h.executor.submits(), 1);
}

#[tokio::test]
async fn transport_errors_leave_the_intent_unknown_until_reconciled() {
    let h = harness(AccountMode::Paper, LivePolicy::default(), false);
    h.executor.transport_error.store(true, Ordering::SeqCst);
    let decided = approved(AccountMode::Paper).await;
    submit_entry(&h, &decided).await.unwrap();
    let id = h.executor.submitted.lock().unwrap()[0].client_order_id();
    assert_eq!(h.gateway.intent_state(id), Some(OrderIntentState::Unknown));
    // A fill cannot be applied to an unknown order; only reconciliation resolves it.
    let fill = BrokerFill {
        client_order_id: id,
        quantity: Quantity::new(dec!(1)).unwrap(),
        price: Price::new(dec!(100)).unwrap(),
        at: at_close(decision_date()),
    };
    assert!(h.gateway.on_fill(fill).await.is_err());
    assert_eq!(
        h.gateway
            .reconcile_intent(id, ReconciledState::Submitted)
            .await,
        Ok(OrderIntentState::Submitted)
    );
}

#[tokio::test]
async fn reconciliation_reports_mismatches_without_trading() {
    let h = harness(AccountMode::Paper, LivePolicy::default(), false);
    let (pm, _) = filled_position(&h).await;
    let before = h.executor.submits();
    let mismatches = pm.reconcile(&[]);
    assert_eq!(mismatches.len(), 1);
    assert_eq!(h.executor.submits(), before);
}

// ---------- INV-14 ----------

#[tokio::test]
async fn invariant_14_live_orders_are_blocked_by_default() {
    let h = harness(AccountMode::Live, LivePolicy::default(), true);
    let decided = approved(AccountMode::Live).await;
    // The Risk Gate already refuses live entries with unverified costs, so
    // check the gateway rule directly as well.
    assert!(decided.authorization.is_none());
    let blocked = check_live_order(
        &LivePolicy::default(),
        RiskEffect::Increasing,
        true,
        Some(StrategyStage::Full),
    );
    if live_orders_compiled() {
        assert_eq!(blocked, Err(LiveBlock::NotProduction));
    } else {
        assert_eq!(blocked, Err(LiveBlock::NotCompiled));
    }
    assert_eq!(h.executor.submits(), 0);
}

#[test]
fn invariant_14_every_live_condition_is_required() {
    let production = LivePolicy {
        environment: Environment::Production,
        live_trading_enabled: true,
    };
    if !live_orders_compiled() {
        assert_eq!(
            check_live_order(
                &production,
                RiskEffect::Increasing,
                true,
                Some(StrategyStage::Full)
            ),
            Err(LiveBlock::NotCompiled)
        );
        return;
    }
    assert_eq!(
        check_live_order(
            &production,
            RiskEffect::Increasing,
            true,
            Some(StrategyStage::Full)
        ),
        Ok(())
    );
    let disabled = LivePolicy {
        live_trading_enabled: false,
        ..production
    };
    assert_eq!(
        check_live_order(
            &disabled,
            RiskEffect::Increasing,
            true,
            Some(StrategyStage::Full)
        ),
        Err(LiveBlock::Disabled)
    );
    assert_eq!(
        check_live_order(
            &production,
            RiskEffect::Increasing,
            false,
            Some(StrategyStage::Full)
        ),
        Err(LiveBlock::NotArmed)
    );
    assert_eq!(
        check_live_order(
            &production,
            RiskEffect::Increasing,
            true,
            Some(StrategyStage::Paper)
        ),
        Err(LiveBlock::StageNotLive)
    );
    // Exits on a live account are never trapped by disarming.
    assert_eq!(
        check_live_order(&disabled, RiskEffect::Reducing, false, None),
        Ok(())
    );
}

// ---------- restore from the journal (ADR 0009) ----------

fn journal_json(h: &Harness) -> Vec<serde_json::Value> {
    h.journal
        .inner
        .entries()
        .iter()
        .map(|e| serde_json::to_value(e).unwrap())
        .collect()
}

#[tokio::test]
async fn invariant_05_the_journal_alone_rebuilds_the_book_and_the_order_ledger() {
    let h = harness(AccountMode::Paper, LivePolicy::default(), false);
    let (pm, id) = filled_position(&h).await;
    let restored =
        qd_app::restore::RestoredState::from_entries(account_id(), journal_json(&h).iter())
            .unwrap();
    restored.check().unwrap();
    assert_eq!(restored.positions, pm.positions());
    // Every acknowledged order keeps the broker's id (the fake uses the intent id).
    for r in &restored.intents {
        assert_eq!(r.broker_order_id, Some(r.intent.id().to_string()));
    }

    // A fresh gateway and manager, rebuilt from the journal only.
    let fresh = harness(AccountMode::Paper, LivePolicy::default(), false);
    fresh.gateway.restore(&restored);
    let pm2 = PositionManager::new(
        fresh.gateway.clone(),
        fresh.journal.clone(),
        fresh.clock.clone(),
    );
    pm2.restore(&restored);
    assert_eq!(pm2.positions(), pm.positions());
    let p = &pm.positions()[0];
    assert_eq!(p.id, id);
    assert_eq!(
        fresh.gateway.open_quantity(p.instrument, p.side),
        h.gateway.open_quantity(p.instrument, p.side)
    );
    // The protective orders are still working, and the exit rule still
    // holds: no exit beyond the open quantity is accepted after a restart.
    assert_eq!(fresh.gateway.working_intents(id).len(), 2);
    assert_eq!(fresh.executor.submits(), 0);
    let update = &mut qd_app::positions::PositionUpdate::default();
    pm2.exit(id, qd_domain::outcome::ExitReason::Manual, update)
        .await;
    assert!(update.rejections.is_empty(), "{update:?}");
    assert_eq!(fresh.executor.submits(), 1);
}

#[tokio::test]
async fn invariant_06_restore_keeps_unanswered_orders_unknown_and_refuses_lost_snapshots() {
    let h = harness(AccountMode::Paper, LivePolicy::default(), false);
    h.executor.transport_error.store(true, Ordering::SeqCst);
    let decided = approved(AccountMode::Paper).await;
    submit_entry(&h, &decided).await.unwrap();
    let restored =
        qd_app::restore::RestoredState::from_entries(account_id(), journal_json(&h).iter())
            .unwrap();
    assert_eq!(restored.intents.len(), 1);
    assert_eq!(restored.intents[0].state, OrderIntentState::Unknown);
    // Another account's restore sees nothing of this one.
    let other = qd_app::restore::RestoredState::from_entries(
        qd_domain::ids::AccountId::new_at(at_close(decision_date())),
        journal_json(&h).iter(),
    )
    .unwrap();
    assert!(other.intents.is_empty());

    // A fill whose position snapshot never reached the journal: refused.
    let h = harness(AccountMode::Paper, LivePolicy::default(), false);
    let _ = filled_position(&h).await;
    let entries: Vec<serde_json::Value> = journal_json(&h)
        .into_iter()
        .filter(|e| e["kind"] != "position_snapshot")
        .collect();
    let restored =
        qd_app::restore::RestoredState::from_entries(account_id(), entries.iter()).unwrap();
    assert!(restored.check().is_err());
}
