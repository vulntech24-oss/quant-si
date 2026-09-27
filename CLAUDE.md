# QuantDesk

A quantitative trading-intelligence platform for one owner: analyze → find
opportunity → predict → decide → validate → manage risk → execute → monitor →
learn. Defined-risk trades lasting days to weeks on completed daily bars, for
Indian stocks, gold, silver, crude oil and crypto. Rust modular monolith
(Tokio, Axum, SQLx, PostgreSQL, Redis). Paper trading is the default everywhere.
NO TRADE is a normal, frequent outcome.

- Spec (source of truth): `docs/QUANTDESK_BUILD_SPEC.md`. It currently ends at
  §6.6; §7–§20 are missing (ADR 0001).
- Decisions: `docs/adr/`. Progress and next steps: `docs/PROGRESS.md`.
- Current phase: **Phase 1 (domain core) done; Phase 2 next**. See `docs/PROGRESS.md`.

## Commands

Run from `backend/`. All must pass before every commit (spec §1.4).

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo deny check            # install once: cargo install cargo-deny --locked
# once SQLx query macros exist:
cargo sqlx prepare --workspace --check
```

Never disable a test or lint to get green. Allow a lint locally only with a
comment explaining why.

## Code map

```
backend/                     Cargo workspace (ADR 0002)
  Cargo.toml                 shared deps and [workspace.lints]
  deny.toml                  cargo-deny policy
  crates/qd-domain/          pure domain core: no I/O, no clock, no floats
    src/ids.rs               UUIDv7 id newtypes (time passed in)
    src/num.rs               Price, Quantity, Money, Currency, Ratio, Bps, FxRate
    src/instrument.rs        InstrumentSpec (data) + tick/quantity rounding
    src/action.rs            TradeAction, Entry/ExitAction, Side, RiskEffect, UI labels
    src/outcome.rs           DecisionOutcome, NoTradeReason, ExitReason, RiskLimitBreach
    src/plan.rs              TradePlan (tick-rounded, validated), PlanDefect
    src/economics.rs         costs, slippage, UnitEconomics, probabilities, EV
    src/sizing.rs            sizing steps 1, 2, 3 (cap helper), 5
    src/portfolio.rs         open risk, daily P&L, drawdown, R-multiple
    src/proposal.rs          TradeProposal (complete or not built)
    src/halt.rs              halts / kill switch, check_order
    src/order_rules.rs       exit-quantity rule
    src/lifecycle/           strategy stage, order intent, position state machines
    tests/                   formulas, lifecycle tables, properties, invariants
design/stitch-reference/     Stitch export: visual reference only, not requirements
docs/                        spec, ADRs, progress log
```

Target crates not yet created (§5.3): qd-strategy, qd-risk, qd-app, qd-backtest,
qd-ai, qd-broker-kite, qd-broker-paper, qd-marketdata, qd-store, qd-api,
qd-server, qd-cli. Create a crate only when it has real code.

## Rules of thumb

- Decimal everywhere for prices, quantities and money; `f64` only inside statistics.
- Pure crates never read the clock; pass time in (clippy bans it in qd-domain).
- Formulas include the contract multiplier and FX (ADR 0003).
- Tick rounding is conservative for the trade; quantities round down.
- Fakes are named `Fake*`/`Mock*`, live in test support or behind `dev-fakes`.
- No `todo!`, `unimplemented!`, `dbg!`, `unwrap`, `expect` in runtime code.
- Never place a real order. Never ask for secret values; name the `.env` variables.
- Read the provider's current docs before any integration; record them in
  `docs/integrations/<provider>.md`.
- Small conventional commits, only when every check passes.

## Invariants (spec §3); tests are named `invariant_NN_*`

- INV-01 One order path: only the Order Gateway can reach order-placing broker calls.
- INV-02 Halts block risk-increasing orders, never exits; exits never exceed open qty.
- INV-03 The Risk Gate can reject any entry; nothing bypasses it, manual entries included.
- INV-04 AI is advisory; it never touches orders, sizes, limits, params, credentials, halts.
- INV-05 No journal, no trade: decisions and intents are durably written before broker calls.
- INV-06 Fail closed on missing/stale/inconsistent inputs; unknown halt state = halted.
- INV-07 Startup keeps entries halted until reconciliation; hard halts need a human re-arm.
- INV-08 Same strategy/risk/gateway/cost code in backtest, paper and live.
- INV-09 Point-in-time data only; every decision replays from an immutable snapshot.
- INV-10 Strategy versions are immutable; only validated versions at trading stages trade.
- INV-11 Promotion needs owner approval with evidence; demotion on breach is automatic.
- INV-12 Actions are OpenLong/CloseLong/OpenShort/CloseShort; no bare BUY/SELL.
- INV-13 Exact decimal arithmetic; levels and quantities rounded before RR/EV/size.
- INV-14 Live orders need the `live-orders` feature, production env, config, `live_armed`, live stage.
- INV-15 Secrets stay on the server; the frontend talks only to the QuantDesk backend.
- INV-16 Journal, order events, halt events, stage transitions and audit logs are append-only.
- INV-17 Every decision display is post-risk; a risk-blocked signal shows NO TRADE + reason.
