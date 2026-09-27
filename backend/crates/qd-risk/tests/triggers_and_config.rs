//! Halt triggers and the shipped risk configuration.

// Test code: a failed unwrap is a failed test. The workspace-wide unwrap/expect
// ban targets runtime code, and clippy's allow-unwrap-in-tests does not reach
// helper functions in integration-test files.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use chrono::Duration;
use qd_domain::halt::{ClearedBy, HaltKind, HaltScope, HaltState};
use qd_domain::ids::{HaltId, UserId};
use qd_domain::num::Ratio;
use qd_domain::proposal::AccountMode;
use qd_risk::config::{RiskConfig, RiskConfigData};
use qd_risk::triggers::halt_triggers;
use rust_decimal_macros::dec;

use common::*;

#[test]
fn shipped_risk_config_holds_the_spec_section_4_defaults() {
    let c = config();
    assert_eq!(c.risk_per_trade.value(), dec!(0.005));
    assert_eq!(c.max_total_open_risk.value(), dec!(0.05));
    assert_eq!(c.max_bucket_open_risk.value(), dec!(0.02));
    assert_eq!(c.max_strategy_open_risk.value(), dec!(0.02));
    assert_eq!(c.daily_loss_limit.value(), dec!(0.02));
    assert_eq!(c.weekly_loss_limit.value(), dec!(0.04));
    assert_eq!(c.hard_halt_drawdown.value(), dec!(0.10));
    assert_eq!(c.cool_off_after_losses, 3);
    assert_eq!(c.stage_multipliers.paper, dec!(0));
    assert_eq!(c.stage_multipliers.small_capital, dec!(0.25));
    assert_eq!(c.stage_multipliers.full, dec!(1.0));
    assert!(c.paper_accounts_size_as_full);
    assert_eq!(c.min_ev_r, dec!(0.10));
    assert_eq!(c.min_evidence, 30);
}

#[test]
fn invalid_risk_configs_are_rejected() {
    let base: RiskConfigData = toml::from_str(RISK).unwrap();

    let mut c = base.clone();
    c.stage_multipliers.paper = dec!(0.1);
    assert!(RiskConfig::new(c).is_err(), "paper must never trade live");

    let mut c = base.clone();
    c.risk_per_trade = Ratio::new(dec!(0.03)).unwrap();
    assert!(
        RiskConfig::new(c).is_err(),
        "per-trade risk above the bucket cap"
    );

    let mut c = base.clone();
    c.max_bucket_open_risk = Ratio::new(dec!(0.06)).unwrap();
    assert!(
        RiskConfig::new(c).is_err(),
        "bucket cap above the total cap"
    );

    let mut c = base.clone();
    c.hard_halt_drawdown = Ratio::new(dec!(1)).unwrap();
    assert!(RiskConfig::new(c).is_err());

    let mut c = base;
    c.cool_off_after_losses = 0;
    assert!(RiskConfig::new(c).is_err());
}

#[test]
fn invariant_07_drawdown_triggers_a_hard_halt_only_a_human_can_clear() {
    let config = config();
    let mut state = account(AccountMode::Live);
    state.high_water_mark = inr(dec!(1200000)); // equity 10,00,000 is 16.7% below
    let now = at(10, 0);
    let proposals = halt_triggers(&state, &config, now).unwrap();
    assert_eq!(proposals.len(), 1);
    let proposal = proposals[0].clone();
    assert_eq!(proposal.kind, HaltKind::HardHalt);
    assert_eq!(proposal.scope, HaltScope::Account(state.account));
    assert!(proposal.requires_manual_rearm);
    assert_eq!(proposal.auto_clear_at, None);

    let halt = proposal.into_halt(HaltId::new_at(now), now).unwrap();
    assert!(halt.clear(ClearedBy::System, now).is_err());
    assert!(
        halt.clear(ClearedBy::Human(UserId::new_at(now)), now)
            .is_ok()
    );

    // Not proposed again while it is active.
    state.halts = HaltState::Known(vec![halt]);
    assert!(halt_triggers(&state, &config, now).unwrap().is_empty());
}

#[test]
fn consecutive_losses_trigger_a_self_clearing_cool_off() {
    let config = config();
    let mut state = account(AccountMode::Paper);
    state.consecutive_losses = 3;
    let now = at(10, 0);
    let proposals = halt_triggers(&state, &config, now).unwrap();
    assert_eq!(proposals.len(), 1);
    let cool_off = proposals[0].clone();
    assert_eq!(cool_off.kind, HaltKind::CoolOff);
    assert_eq!(cool_off.auto_clear_at, Some(now + Duration::hours(24)));
    let halt = cool_off.into_halt(HaltId::new_at(now), now).unwrap();
    assert!(halt.is_active_at(now + Duration::hours(23)));
    assert!(!halt.is_active_at(now + Duration::hours(24)));
}

#[test]
fn a_healthy_account_triggers_nothing_and_an_inconsistent_one_errors() {
    let config = config();
    let state = account(AccountMode::Paper);
    assert!(
        halt_triggers(&state, &config, at(10, 0))
            .unwrap()
            .is_empty()
    );

    let mut stale = account(AccountMode::Paper);
    stale.equity = inr(dec!(1000001)); // above the high-water mark
    assert!(halt_triggers(&stale, &config, at(10, 0)).is_err());

    // With the halt state unknown, a due hard halt is still proposed.
    let mut unknown = account(AccountMode::Paper);
    unknown.high_water_mark = inr(dec!(2000000));
    unknown.halts = HaltState::Unknown;
    assert_eq!(
        halt_triggers(&unknown, &config, at(10, 0)).unwrap().len(),
        1
    );
}
