//! PostgreSQL adapters against a real database. `#[sqlx::test]` creates a fresh
//! database per test from `DATABASE_URL` and applies the migrations.

// Test code: a failed unwrap is a failed test. The workspace-wide unwrap/expect
// ban targets runtime code, and clippy's allow-unwrap-in-tests does not reach
// helper functions in integration-test files.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use chrono::{Duration, NaiveDate, TimeZone, Utc};
use qd_app::evidence::{EvidenceKind, EvidenceRecord, EvidenceStore};
use qd_app::journal::JournalEntry;
use qd_app::ports::{HaltStore, HistoricalMarketData, Journal};
use qd_app::registry::{RegistryError, StrategyRegistry, StrategyVersionRecord};
use qd_domain::halt::{ClearedBy, Halt, HaltKind, HaltScope};
use qd_domain::ids::{
    AccountId, EvidenceId, HaltId, InstrumentId, StrategyId, StrategyVersionId, UserId,
};
use qd_domain::lifecycle::strategy::{OwnerApproval, StageEvent, StrategyStage, TradingStage};
use qd_domain::market::{Bar, BarData};
use qd_domain::proposal::{AccountMode, StrategyRef};
use qd_store::{
    AccountRecord, PgAccounts, PgAuditLog, PgEvidence, PgHaltStore, PgJournal, PgMarketData,
    PgStrategyRegistry,
};
use rust_decimal_macros::dec;
use sqlx::PgPool;

fn at(h: u32) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 3, 16, h, 0, 0).unwrap()
}

fn halt_entry() -> JournalEntry {
    let halt = Halt::new(
        HaltId::new_at(at(9)),
        HaltKind::Manual,
        HaltScope::Global,
        "test",
        at(9),
        None,
        true,
    )
    .unwrap();
    JournalEntry::Halt {
        halt: Box::new(halt),
    }
}

async fn mutation_fails(pool: &PgPool, sql: &str) {
    let err = sqlx::query(sql).execute(pool).await.unwrap_err();
    assert!(err.to_string().contains("append-only"), "{sql}: {err}");
}

#[sqlx::test(migrator = "qd_store::MIGRATOR")]
async fn invariant_16_history_tables_reject_updates_deletes_and_truncates(pool: PgPool) {
    let journal = PgJournal::new(pool.clone());
    let first = journal.append(&halt_entry()).await.unwrap();
    let second = journal.append(&halt_entry()).await.unwrap();
    assert!(second > first);
    for table in [
        "journal",
        "halt_events",
        "instrument_specs",
        "bars",
        "strategy_versions",
        "strategy_stage_events",
        "audit_log",
    ] {
        // TRUNCATE is refused by a statement trigger, even on an empty table.
        mutation_fails(&pool, &format!("TRUNCATE {table} CASCADE")).await;
    }
    // Row triggers refuse every UPDATE or DELETE that touches a row.
    mutation_fails(&pool, "DELETE FROM journal").await;
    mutation_fails(&pool, "UPDATE journal SET kind = 'x'").await;
    assert_eq!(journal.recent(10).await.unwrap().len(), 2);
}

#[sqlx::test(migrator = "qd_store::MIGRATOR")]
async fn invariant_07_halts_survive_in_the_store_and_are_revalidated(pool: PgPool) {
    let store = PgHaltStore::new(pool.clone());
    let hard = Halt::new(
        HaltId::new_at(at(9)),
        HaltKind::HardHalt,
        HaltScope::Global,
        "drawdown",
        at(9),
        None,
        true,
    )
    .unwrap();
    store.record(&hard).await.unwrap();
    assert_eq!(store.load().await.unwrap(), vec![hard.clone()]);
    // A new store over the same database (a restart) still sees it.
    assert_eq!(
        PgHaltStore::new(pool.clone()).load().await.unwrap(),
        vec![hard.clone()]
    );

    let cleared = hard
        .clear(ClearedBy::Human(UserId::new_at(at(9))), at(10))
        .unwrap();
    store.record(&cleared).await.unwrap();
    assert_eq!(store.load().await.unwrap(), vec![cleared]);

    // A row claiming the system cleared a hard halt is refused on read: fail closed.
    let tampered = serde_json::json!({
        "id": HaltId::new_at(at(11)), "kind": "hard_halt", "scope": {"scope": "global"},
        "reason": "x", "started_at": at(11), "auto_clear_at": null, "requires_manual_rearm": true,
        "cleared_at": at(12), "cleared_by": {"by": "system"}
    });
    sqlx::query("INSERT INTO halt_events (halt_id, halt) VALUES ($1, $2)")
        .bind(uuid::Uuid::now_v7())
        .bind(tampered)
        .execute(&pool)
        .await
        .unwrap();
    assert!(store.load().await.is_err());
}

fn bar(date: NaiveDate, close: rust_decimal::Decimal) -> Bar {
    Bar::new(BarData {
        date,
        open: close,
        high: close + dec!(1),
        low: close - dec!(1),
        close,
        volume: dec!(100),
    })
    .unwrap()
}

#[sqlx::test(migrator = "qd_store::MIGRATOR")]
async fn invariant_09_bar_corrections_are_invisible_to_earlier_snapshots(pool: PgPool) {
    let market = PgMarketData::new(pool);
    let id = InstrumentId::new_at(at(0));
    let d = NaiveDate::from_ymd_opt(2026, 3, 13).unwrap();
    market
        .insert_bars(id, &[bar(d, dec!(100))], at(1))
        .await
        .unwrap();
    market
        .insert_bars(id, &[bar(d, dec!(105))], at(5))
        .await
        .unwrap();
    let before = market.daily_bars(id, d, d, at(3)).await.unwrap();
    let after = market.daily_bars(id, d, d, at(6)).await.unwrap();
    assert_eq!(before[0].close().value(), dec!(100));
    assert_eq!(after[0].close().value(), dec!(105));
    assert!(market.daily_bars(id, d, d, at(0)).await.unwrap().is_empty());
}

fn version() -> StrategyVersionRecord {
    StrategyVersionRecord {
        reference: StrategyRef {
            strategy_id: StrategyId::new_at(at(0)),
            name: "Trend pullback".to_owned(),
            version_id: StrategyVersionId::new_at(at(0)),
            version_number: 1,
            logic_version: "trend-pullback-1.0.0".to_owned(),
            git_sha: "abc".to_owned(),
        },
        parameters: serde_json::json!({"stop_atr": "2"}),
        rr_floor: dec!(1.5),
    }
}

#[sqlx::test(migrator = "qd_store::MIGRATOR")]
async fn invariant_10_versions_are_immutable_and_stages_replay(pool: PgPool) {
    let registry = registry(&pool);
    let v = version();
    let id = v.reference.version_id;
    registry.register(&v, "owner").await.unwrap();
    let evidence = record_evidence(&pool, id, EvidenceKind::Validation, true).await;
    assert!(
        registry.register(&v, "owner").await.is_err(),
        "duplicate id"
    );
    mutation_fails(&pool, "UPDATE strategy_versions SET rr_floor = 0.5").await;

    assert_eq!(registry.stage(id).await.unwrap(), StrategyStage::Draft);
    registry
        .transition(id, &StageEvent::StartResearch, "system")
        .await
        .unwrap();
    registry
        .transition(id, &StageEvent::PassResearch { evidence }, "system")
        .await
        .unwrap();
    // Skipping Paper is illegal and nothing is stored.
    let approval = OwnerApproval {
        approved_by: UserId::new_at(at(1)),
        approved_at: at(2),
        evidence,
    };
    assert!(
        registry
            .transition(
                id,
                &StageEvent::Promote {
                    to: TradingStage::Full,
                    approval
                },
                "owner"
            )
            .await
            .is_err()
    );
    let stage = registry
        .transition(
            id,
            &StageEvent::Promote {
                to: TradingStage::Paper,
                approval,
            },
            "owner",
        )
        .await
        .unwrap();
    assert_eq!(stage, StrategyStage::Paper);
    let (loaded, stage) = registry.get(id).await.unwrap();
    assert_eq!(loaded, v);
    assert_eq!(stage, StrategyStage::Paper);
    let audits: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(audits, 4);
}

fn registry(pool: &PgPool) -> StrategyRegistry {
    StrategyRegistry::new(
        Arc::new(PgStrategyRegistry::new(pool.clone())),
        Arc::new(PgAuditLog::new(pool.clone())),
        Arc::new(PgEvidence::new(pool.clone())),
    )
}

async fn record_evidence(
    pool: &PgPool,
    version: StrategyVersionId,
    kind: EvidenceKind,
    passed: bool,
) -> EvidenceId {
    let record = EvidenceRecord {
        id: EvidenceId::new_at(at(1)),
        version,
        kind,
        passed,
        report: serde_json::json!({"evidence": []}),
        created_at: at(1),
    };
    PgEvidence::new(pool.clone()).record(&record).await.unwrap();
    record.id
}

#[sqlx::test(migrator = "qd_store::MIGRATOR")]
async fn invariant_11_stage_events_need_recorded_passing_evidence_of_the_right_kind(pool: PgPool) {
    let registry = registry(&pool);
    let v = version();
    let id = v.reference.version_id;
    registry.register(&v, "owner").await.unwrap();
    registry
        .transition(id, &StageEvent::StartResearch, "system")
        .await
        .unwrap();
    let pass = |evidence| StageEvent::PassResearch { evidence };
    // Unrecorded, failed, or the wrong kind: refused, and nothing is stored.
    let unrecorded = EvidenceId::new_at(at(1));
    let failed = record_evidence(&pool, id, EvidenceKind::Validation, false).await;
    let review = record_evidence(&pool, id, EvidenceKind::PaperReview, true).await;
    for evidence in [unrecorded, failed, review] {
        let err = registry.transition(id, &pass(evidence), "owner").await;
        assert!(matches!(err, Err(RegistryError::Evidence(_))), "{err:?}");
    }
    // Another version's evidence is refused too.
    let mut other = version();
    other.reference.version_id = StrategyVersionId::new_at(at(3));
    other.reference.version_number = 2;
    registry.register(&other, "owner").await.unwrap();
    let foreign = record_evidence(
        &pool,
        other.reference.version_id,
        EvidenceKind::Validation,
        true,
    )
    .await;
    assert!(
        registry
            .transition(id, &pass(foreign), "owner")
            .await
            .is_err()
    );
    assert_eq!(registry.stage(id).await.unwrap(), StrategyStage::Research);

    let validation = record_evidence(&pool, id, EvidenceKind::Validation, true).await;
    registry
        .transition(id, &pass(validation), "owner")
        .await
        .unwrap();
    let promote = |to, evidence| StageEvent::Promote {
        to,
        approval: OwnerApproval {
            approved_by: UserId::new_at(at(1)),
            approved_at: at(2),
            evidence,
        },
    };
    registry
        .transition(id, &promote(TradingStage::Paper, validation), "owner")
        .await
        .unwrap();
    // A live stage needs a passed paper review; a validation is not enough.
    assert!(
        registry
            .transition(
                id,
                &promote(TradingStage::SmallCapital, validation),
                "owner"
            )
            .await
            .is_err()
    );
    assert_eq!(
        registry
            .transition(id, &promote(TradingStage::SmallCapital, review), "owner")
            .await
            .unwrap(),
        StrategyStage::SmallCapital
    );
    // Evidence is append-only.
    mutation_fails(&pool, "UPDATE evidence_records SET passed = true").await;
}

#[sqlx::test(migrator = "qd_store::MIGRATOR")]
async fn invariant_14_only_live_accounts_can_be_armed_and_arming_is_audited(pool: PgPool) {
    let accounts = PgAccounts::new(pool.clone());
    let paper = AccountRecord {
        id: AccountId::new_at(at(0)),
        name: "paper".to_owned(),
        mode: AccountMode::Paper,
        currency: "INR".to_owned(),
        live_armed: false,
    };
    accounts.create(&paper, "owner").await.unwrap();
    assert!(
        accounts
            .set_live_armed(paper.id, true, "owner")
            .await
            .is_err()
    );
    assert!(!accounts.get(paper.id).await.unwrap().unwrap().live_armed);

    let live = AccountRecord {
        id: AccountId::new_at(at(1)),
        name: "live".to_owned(),
        mode: AccountMode::Live,
        ..paper
    };
    accounts.create(&live, "owner").await.unwrap();
    assert!(
        !accounts.get(live.id).await.unwrap().unwrap().live_armed,
        "never armed at creation"
    );
    accounts
        .set_live_armed(live.id, true, "owner")
        .await
        .unwrap();
    assert!(accounts.get(live.id).await.unwrap().unwrap().live_armed);
    let armed_audits: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = 'account.live_armed'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(armed_audits, 1);
    let _ = Duration::zero();
}
