//! Invariant tests (spec §3) for the parts that live in the domain core.
//!
//! Invariants that need other components (the Order Gateway, the journal, the
//! database) get their own `invariant_NN_*` tests in the crates that build them.

// Test code: a failed unwrap is a failed test. The workspace-wide unwrap/expect
// ban targets runtime code, and clippy's allow-unwrap-in-tests does not reach
// helper functions in integration-test files.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::Path;

use chrono::Duration;
use qd_domain::action::{EntryAction, ExitAction, RiskEffect, Side, TradeAction};
use qd_domain::halt::{
    ClearedBy, Halt, HaltBlock, HaltError, HaltKind, HaltScope, HaltState, OrderContext,
    check_order,
};
use qd_domain::ids::{AccountId, EvidenceId, HaltId, InstrumentId, StrategyVersionId, UserId};
use qd_domain::lifecycle::strategy::{OwnerApproval, StageEvent, StrategyStage, TradingStage};
use qd_domain::num::{Quantity, Ratio};
use qd_domain::order_rules::{ExitQuantityError, check_exit_quantity};
use qd_domain::outcome::{DecisionOutcome, ExitReason, NoTradeReason, RiskLimitBreach};
use qd_domain::plan::{EntryOrderType, TradePlan, TradePlanInput};
use rust_decimal_macros::dec;

fn context() -> OrderContext {
    OrderContext {
        account: AccountId::new_at(common::at(0, 0)),
        strategy_version: Some(StrategyVersionId::new_at(common::at(0, 0))),
        instrument: InstrumentId::new_at(common::at(0, 0)),
    }
}

fn halt(kind: HaltKind, scope: HaltScope) -> Halt {
    Halt::new(
        HaltId::new_at(common::at(9, 0)),
        kind,
        scope,
        "test halt",
        common::at(9, 0),
        None,
        kind.always_requires_manual_rearm() || kind == HaltKind::Operational,
    )
    .unwrap()
}

fn every_kind() -> [HaltKind; 5] {
    [
        HaltKind::Manual,
        HaltKind::CoolOff,
        HaltKind::HardHalt,
        HaltKind::Operational,
        HaltKind::Startup,
    ]
}

fn every_covering_scope(ctx: &OrderContext) -> Vec<HaltScope> {
    vec![
        HaltScope::Global,
        HaltScope::Account(ctx.account),
        HaltScope::StrategyVersion(ctx.strategy_version.unwrap()),
        HaltScope::Instrument(ctx.instrument),
    ]
}

// ---------- INV-02 ----------

#[test]
fn invariant_02_halts_never_block_risk_reducing_orders() {
    let ctx = context();
    let now = common::at(10, 0);
    for kind in every_kind() {
        for scope in every_covering_scope(&ctx) {
            let state = HaltState::Known(vec![halt(kind, scope)]);
            assert_eq!(check_order(RiskEffect::Reducing, &state, &ctx, now), Ok(()));
        }
    }
    assert_eq!(
        check_order(RiskEffect::Reducing, &HaltState::Unknown, &ctx, now),
        Ok(())
    );
    for action in [TradeAction::CloseLong, TradeAction::CloseShort] {
        assert_eq!(action.risk_effect(), RiskEffect::Reducing);
    }
}

#[test]
fn invariant_02_halts_block_risk_increasing_orders_they_cover() {
    let ctx = context();
    let now = common::at(10, 0);
    for kind in every_kind() {
        for scope in every_covering_scope(&ctx) {
            let h = halt(kind, scope);
            let state = HaltState::Known(vec![h.clone()]);
            assert_eq!(
                check_order(RiskEffect::Increasing, &state, &ctx, now),
                Err(HaltBlock::Active {
                    halt: h.id(),
                    kind,
                    scope
                })
            );
        }
    }
}

#[test]
fn invariant_02_halts_do_not_block_orders_outside_their_scope() {
    let ctx = context();
    let other = common::at(1, 0);
    let unrelated = [
        HaltScope::Account(AccountId::new_at(other)),
        HaltScope::StrategyVersion(StrategyVersionId::new_at(other)),
        HaltScope::Instrument(InstrumentId::new_at(other)),
    ];
    for scope in unrelated {
        let state = HaltState::Known(vec![halt(HaltKind::Manual, scope)]);
        assert_eq!(
            check_order(RiskEffect::Increasing, &state, &ctx, common::at(10, 0)),
            Ok(())
        );
    }
    // A manual order has no strategy version, so a strategy-version halt does not cover it.
    let manual = OrderContext {
        strategy_version: None,
        ..ctx
    };
    let state = HaltState::Known(vec![halt(
        HaltKind::Manual,
        HaltScope::StrategyVersion(ctx.strategy_version.unwrap()),
    )]);
    assert_eq!(
        check_order(RiskEffect::Increasing, &state, &manual, common::at(10, 0)),
        Ok(())
    );
}

#[test]
fn invariant_02_exit_can_never_exceed_the_open_quantity() {
    let open = Quantity::new(dec!(100)).unwrap();
    assert_eq!(
        check_exit_quantity(open, Quantity::new(dec!(100)).unwrap()),
        Ok(())
    );
    assert_eq!(
        check_exit_quantity(open, Quantity::new(dec!(40)).unwrap()),
        Ok(())
    );
    assert_eq!(
        check_exit_quantity(open, Quantity::new(dec!(100.00001)).unwrap()),
        Err(ExitQuantityError::ExceedsOpen {
            open,
            exit: Quantity::new(dec!(100.00001)).unwrap()
        })
    );
    assert_eq!(
        check_exit_quantity(open, Quantity::ZERO),
        Err(ExitQuantityError::Zero)
    );
    assert_eq!(
        check_exit_quantity(Quantity::ZERO, Quantity::new(dec!(1)).unwrap()),
        Err(ExitQuantityError::ExceedsOpen {
            open: Quantity::ZERO,
            exit: Quantity::new(dec!(1)).unwrap()
        })
    );
}

// ---------- INV-06 ----------

#[test]
fn invariant_06_unknown_kill_switch_state_counts_as_halted() {
    assert_eq!(
        check_order(
            RiskEffect::Increasing,
            &HaltState::Unknown,
            &context(),
            common::at(10, 0)
        ),
        Err(HaltBlock::StateUnknown)
    );
}

// ---------- INV-07 ----------

#[test]
fn invariant_07_hard_and_manual_halts_need_a_human_to_rearm() {
    for kind in [HaltKind::HardHalt, HaltKind::Manual] {
        let h = halt(kind, HaltScope::Global);
        assert_eq!(
            h.clear(ClearedBy::System, common::at(10, 0)),
            Err(HaltError::HumanRearmRequired)
        );
        let owner = UserId::new_at(common::at(0, 0));
        let cleared = h.clear(ClearedBy::Human(owner), common::at(10, 0)).unwrap();
        assert!(!cleared.is_active_at(common::at(10, 0)));
        assert!(
            h.is_active_at(common::at(10, 0)),
            "the original stays unchanged"
        );

        // They can never be created as auto-clearing or system-clearable.
        let start = common::at(9, 0);
        let id = HaltId::new_at(start);
        assert_eq!(
            Halt::new(
                id,
                kind,
                HaltScope::Global,
                "x",
                start,
                Some(start + Duration::hours(1)),
                true
            ),
            Err(HaltError::KindRequiresManualRearm(kind))
        );
        assert_eq!(
            Halt::new(id, kind, HaltScope::Global, "x", start, None, false),
            Err(HaltError::KindRequiresManualRearm(kind))
        );
    }
}

#[test]
fn invariant_07_startup_halt_blocks_entries_until_cleared() {
    let startup = halt(HaltKind::Startup, HaltScope::Global);
    let ctx = context();
    let state = HaltState::Known(vec![startup.clone()]);
    assert!(check_order(RiskEffect::Increasing, &state, &ctx, common::at(9, 30)).is_err());

    // Reconciliation and health checks passed: the system clears it.
    let cleared = startup.clear(ClearedBy::System, common::at(9, 31)).unwrap();
    let state = HaltState::Known(vec![cleared]);
    assert_eq!(
        check_order(RiskEffect::Increasing, &state, &ctx, common::at(9, 31)),
        Ok(())
    );
}

#[test]
fn cool_off_halts_clear_automatically() {
    let start = common::at(9, 0);
    let cool_off = Halt::new(
        HaltId::new_at(start),
        HaltKind::CoolOff,
        HaltScope::Global,
        "3 consecutive losses",
        start,
        Some(start + Duration::hours(24)),
        false,
    )
    .unwrap();
    assert!(cool_off.is_active_at(start + Duration::hours(23)));
    assert!(!cool_off.is_active_at(start + Duration::hours(24)));
}

// ---------- INV-10 / INV-11 / INV-14 ----------

fn approval() -> OwnerApproval {
    OwnerApproval {
        approved_by: UserId::new_at(common::at(0, 0)),
        approved_at: common::at(12, 0),
        evidence: EvidenceId::new_at(common::at(11, 0)),
    }
}

#[test]
fn invariant_10_only_validated_versions_at_a_trading_stage_can_trade() {
    let not_trading = [
        StrategyStage::Draft,
        StrategyStage::Research,
        StrategyStage::Rejected,
        StrategyStage::ResearchPassed,
        StrategyStage::Suspended {
            resume_to: TradingStage::Full,
        },
        StrategyStage::Retired,
    ];
    for stage in not_trading {
        assert!(!stage.can_trade(), "{stage:?} must not trade");
        assert!(!stage.is_live_eligible());
    }
    assert!(StrategyStage::Paper.can_trade());
    assert!(
        !StrategyStage::Paper.is_live_eligible(),
        "INV-14: paper is never live"
    );
    assert!(StrategyStage::SmallCapital.is_live_eligible());
    assert!(StrategyStage::Full.is_live_eligible());
}

#[test]
fn invariant_11_promotion_is_one_approved_step_at_a_time() {
    let promote = |to| StageEvent::Promote {
        to,
        approval: approval(),
    };
    let stage = StrategyStage::Draft
        .apply(&StageEvent::StartResearch)
        .and_then(|s| {
            s.apply(&StageEvent::PassResearch {
                evidence: EvidenceId::new_at(common::at(11, 0)),
            })
        })
        .and_then(|s| s.apply(&promote(TradingStage::Paper)))
        .unwrap();
    assert_eq!(stage, StrategyStage::Paper);

    // Skipping a stage is illegal.
    assert!(stage.apply(&promote(TradingStage::Full)).is_err());
    let stage = stage.apply(&promote(TradingStage::SmallCapital)).unwrap();
    let stage = stage.apply(&promote(TradingStage::Full)).unwrap();
    assert_eq!(stage, StrategyStage::Full);
    // Nothing above Full.
    assert!(stage.apply(&promote(TradingStage::Full)).is_err());
    // Research cannot be skipped either.
    assert!(
        StrategyStage::Draft
            .apply(&promote(TradingStage::Paper))
            .is_err()
    );
    assert!(
        StrategyStage::Research
            .apply(&promote(TradingStage::Paper))
            .is_err()
    );
}

#[test]
fn invariant_11_demotion_is_automatic_and_needs_no_approval() {
    let breach = StageEvent::AutoDemote {
        reason: "live drawdown exceeded the version's halt threshold".to_owned(),
    };
    for stage in [StrategyStage::SmallCapital, StrategyStage::Full] {
        assert_eq!(stage.apply(&breach).unwrap(), StrategyStage::Paper);
    }
    assert!(StrategyStage::Paper.apply(&breach).is_err());
}

// ---------- INV-12 ----------

#[test]
fn invariant_12_ui_labels_say_exactly_which_action() {
    assert_eq!(TradeAction::OpenLong.label(), "BUY (open long)");
    assert_eq!(TradeAction::CloseLong.label(), "SELL (close long)");
    assert_eq!(TradeAction::OpenShort.label(), "SELL SHORT (open short)");
    assert_eq!(
        TradeAction::CloseShort.label(),
        "BUY TO COVER (close short)"
    );
    assert_eq!(DecisionOutcome::Hold.headline(), "HOLD");
    assert_eq!(
        DecisionOutcome::Enter {
            action: EntryAction::OpenShort
        }
        .headline(),
        "SELL SHORT (open short)"
    );
    assert_eq!(
        DecisionOutcome::Exit {
            action: ExitAction::CloseLong,
            reason: ExitReason::StopHit
        }
        .headline(),
        "SELL (close long)"
    );
}

#[test]
fn invariant_12_entries_and_exits_cannot_be_confused() {
    // `Enter` only accepts `EntryAction` and `Exit` only `ExitAction`; the types
    // make an exit-labelled entry unrepresentable. Check the mapping back as well.
    for side in [Side::Long, Side::Short] {
        assert_eq!(
            TradeAction::from(side.entry()).risk_effect(),
            RiskEffect::Increasing
        );
        assert_eq!(
            TradeAction::from(side.exit()).risk_effect(),
            RiskEffect::Reducing
        );
    }
    let json = serde_json::to_value(DecisionOutcome::Enter {
        action: EntryAction::OpenLong,
    })
    .unwrap();
    assert_eq!(json["outcome"], "enter");
    assert_eq!(json["action"], "open_long");
}

fn domain_sources() -> Vec<(String, String)> {
    fn walk(dir: &Path, out: &mut Vec<(String, String)>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = std::fs::read_to_string(&path).unwrap();
                out.push((path.display().to_string(), text));
            }
        }
    }
    let mut out = Vec::new();
    walk(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut out);
    assert!(!out.is_empty());
    out
}

fn has_word(text: &str, word: &str) -> bool {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .any(|token| token == word)
}

#[test]
fn invariant_12_no_bare_buy_or_sell_identifiers_in_the_domain() {
    for (path, text) in domain_sources() {
        for word in ["Buy", "Sell", "BUY", "SELL", "buy", "sell"] {
            // The only allowed uses are inside the explicit UI labels.
            let without_labels = text
                .replace("\"BUY (open long)\"", "")
                .replace("\"SELL (close long)\"", "")
                .replace("\"SELL SHORT (open short)\"", "")
                .replace("\"BUY TO COVER (close short)\"", "");
            assert!(
                !has_word(&without_labels, word),
                "{path} uses the ambiguous word {word:?}"
            );
        }
    }
}

// ---------- INV-13 ----------

#[test]
fn invariant_13_no_binary_floats_in_the_domain() {
    for (path, text) in domain_sources() {
        for word in ["f32", "f64"] {
            assert!(!has_word(&text, word), "{path} uses {word}");
        }
    }
}

#[test]
fn invariant_13_levels_are_tick_rounded_before_economics() {
    let spec = common::equity();
    let plan = TradePlan::new(
        TradePlanInput {
            action: EntryAction::OpenLong,
            entry_type: EntryOrderType::Limit,
            entry: dec!(100.01),
            stop: dec!(95.04),
            target: dec!(110.09),
            max_holding_days: 5,
            invalidation: vec![],
        },
        &spec,
    )
    .unwrap();
    for level in [plan.entry().price, plan.stop(), plan.target()] {
        assert!(spec.is_tick_aligned(level), "{level} is not a whole tick");
    }
    // Economics use the rounded levels: 100.05 − 95.00.
    assert_eq!(plan.risk_points(), dec!(5.05));
}

#[test]
fn invariant_13_quantities_are_rounded_down_to_the_step() {
    let crypto = common::crypto_spot();
    assert_eq!(
        crypto
            .round_quantity_down(dec!(0.123456789))
            .unwrap()
            .value(),
        dec!(0.12345)
    );
    let crude = common::crude_future();
    assert_eq!(
        crude.round_quantity_down(dec!(1.99)).unwrap().value(),
        dec!(1)
    );
    assert!(crude.is_valid_order_quantity(Quantity::new(dec!(1)).unwrap()));
    assert!(!crude.is_valid_order_quantity(Quantity::new(dec!(1.5)).unwrap()));
    assert!(crude.round_quantity_down(dec!(-1)).is_err());
}

// ---------- INV-17 ----------

#[test]
fn invariant_17_a_risk_blocked_signal_headlines_as_no_trade_with_its_reason() {
    let blocked = DecisionOutcome::NoTrade {
        reason: NoTradeReason::RiskLimit(RiskLimitBreach::TotalOpenRisk {
            limit: Ratio::new(dec!(0.05)).unwrap(),
            would_be: Ratio::new(dec!(0.055)).unwrap(),
        }),
    };
    assert_eq!(blocked.headline(), "NO TRADE");
    let json = serde_json::to_value(&blocked).unwrap();
    assert_eq!(json["outcome"], "no_trade");
    assert_eq!(json["reason"]["code"], "risk_limit");
    assert_eq!(json["reason"]["detail"]["breach"], "total_open_risk");

    let halted = DecisionOutcome::NoTrade {
        reason: NoTradeReason::KillSwitch(HaltBlock::StateUnknown),
    };
    assert_eq!(halted.headline(), "NO TRADE");
}
