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
- Current phase: **Phase 6 (paper trading, restore from the journal) done; Phase 7 next**.
  See `docs/PROGRESS.md`.

## Commands

Run from `backend/`. All must pass before every commit (spec §1.4).

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo deny check            # install once: cargo install cargo-deny --locked
```

Tests need PostgreSQL: `export DATABASE_URL=postgres://USER@localhost:5432/DB` (the user
must be able to create databases; `#[sqlx::test]` makes one per test). Queries are
runtime-checked, so `cargo sqlx prepare` does not apply (ADR 0007).

Run: `qd migrate`, `qd user create --username NAME --role owner` (password on stdin),
then `qd-server` with `QD_CONFIG` (see `config/quantdesk.example.toml`; set `frontend_dir`
to serve the UI) and `QD_DATABASE_URL`. Paper trading: import bars (`qd bars import`),
then `qd paper run --config <file>` (or `[paper] daily_run_utc`). `qd --help` lists
admin commands.

Frontend, from `frontend/` (all must pass too):

```sh
npm ci
npm run typecheck
npm test
npm run build              # dist/, served by qd-server; `npm run dev` proxies /api to :8080
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
    src/economics.rs         CostEstimate, slippage, UnitEconomics, probabilities, EV
    src/costs.rs             CostModel port + versioned schedule-based cost model
    src/market.rs            Bar, BarSeries (completed bars only)
    src/sizing.rs            sizing steps 1, 2, 3 (cap helper), 5
    src/portfolio.rs         open risk, daily P&L, drawdown, R-multiple
    src/proposal.rs          TradeProposal (complete or not built)
    src/halt.rs              halts / kill switch, check_order
    src/order_rules.rs       exit-quantity rule
    src/lifecycle/           strategy stage, order intent, position state machines
    tests/                   formulas, costs, market, lifecycle tables, properties, invariants
  crates/qd-risk/            Risk Gate (pure)
    src/config.rs            RiskConfig (validated)
    src/gate.rs              RiskGate::evaluate → Approved(size) | Rejected(NoTradeReason)
    src/triggers.rs          hard-halt and cool-off triggers
    tests/                   gate, triggers/config, end-to-end pipeline
  crates/qd-strategy/        strategies (pure)
    src/features.rs          features-v1 (SMA, Wilder ATR, highs/lows, ROC)
    src/regime.rs            regime-v1 classifier
    src/strategy.rs          Strategy trait, run_strategy (features → regime → strategy)
    src/trend_pullback.rs    trend-pullback-1.0.0 (long only)
  crates/qd-app/             use cases + ports (async)
    src/ports.rs             Clock, Journal, HaltStore, BrokerOrderExecutor, BrokerAccountReader, EvidenceSource
    src/orders.rs            OrderIntent, EntryAuthorization, BrokerOrderRequest (crate-private constructors)
    src/decision.rs          Trade Proposal Engine + Decision Engine (journal before authorization)
    src/gateway.rs           Order Gateway: the only order path
    src/positions.rs         Position Manager: OCO protection, exits, reconciliation
    src/live.rs              live gate (INV-14); `live-orders` feature, off by default
    src/memory.rs            in-memory journal and halt store (backtests, tests)
    src/registry.rs          Strategy Registry (stage = replay of stored events)
    src/session.rs           daily trading cycle shared by backtest and paper (INV-08)
    src/restore.rs           rebuild gateway/positions/book from the journal; fail closed
  crates/qd-broker-paper/    PaperBroker (daily-bar fill rules, restorable), PaperRunner
  crates/qd-backtest/        run_backtest (loop over the session), research runner, metrics
  crates/qd-store/           PostgreSQL adapters (journal, halts, market data, registry, audit, accounts)
  crates/qd-server/          config (secrets from env), startup (INV-07), /health, /ready; bin qd-server
  crates/qd-cli/             bin `qd`: migrate, accounts, users, instruments, bars, strategy, halts, backtest
  crates/qd-api/             HTTP API under /api (ADR 0008)
    src/auth.rs              argon2id, session cookie, Caller extractor, step-up, login throttle
    src/routes.rs            handlers: parse, authorize, call a port/use case, map
    src/dto.rs               post-risk decision summaries (INV-17)
    openapi.yaml             hand-written API description, served at /api/openapi.yaml
    tests/api.rs             auth, CSRF, step-up, INV-07/14/17 through HTTP
  migrations/                SQL schema; history tables are append-only by trigger (INV-16)
  config/                    data, not code
    costs/india-zerodha.toml cost schedules (UNVERIFIED, see ADR 0005)
    risk.toml                §4 default risk configuration
    quantdesk.example.toml   server settings (no secrets)
    instruments/examples/    example instrument spec (illustrative terms)
frontend/                    Vite + strict TypeScript, no framework (ADR 0008)
  src/format.ts              decimal strings → display (BigInt, Indian grouping), IST times
  src/labels.ts              §6.3 action labels and NO TRADE reason text
  src/api.ts, dom.ts         API client (CSRF header), textContent-only DOM builder
  src/views.ts, main.ts      screens, top bar (PAPER/LIVE, halted), hash router
  tests/                     vitest
design/stitch-reference/     Stitch export: visual reference only, not requirements
docs/                        spec, ADRs, progress log, integrations/ (provider notes)
.github/workflows/ci.yml     runs the commands above
```

Target crates not yet created (§5.3): qd-ai, qd-broker-kite, qd-marketdata (provider docs
unreachable so far: `docs/integrations/`).
Create a crate only when it has real code.

## Rules of thumb

- Decimal everywhere for prices, quantities and money; `f64` only inside statistics.
- Pure crates never read the clock; pass time in (each pure crate's clippy.toml bans it).
- Rates, limits and thresholds are config/data; strategy parameters belong to the logic version.
- Formulas include the contract multiplier and FX (ADR 0003).
- Tick rounding is conservative for the trade; quantities round down.
- Fakes are named `Fake*`/`Mock*`, live in test support or behind `dev-fakes`.
- The journal is the source of truth for trading state; never add mutable position or
  order tables. Every Position Manager change journals a snapshot (ADR 0009).
- Only the Decision Engine can create an `EntryAuthorization`; only the Order Gateway
  can create a `BrokerOrderRequest`. Keep those constructors `pub(crate)`.
- Frontend: never `innerHTML`; never parse decimals into JS numbers; dangerous actions
  need step-up on the server, not just a UI prompt.
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
