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
- Current phase: **Phases 0–9, Zerodha/live/AI/Telegram (ADR 0014), the upgrades
  (ADR 0015) and the AI agent as the central intelligence (ADR 0016) done. Remaining: crypto venue, 2027 holiday lists, owner decisions**
  (`docs/PROGRESS.md`, "What remains").
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
    src/calendar.rs          trading calendars (data), check_bars (gaps, holiday bars, jumps)
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
    src/trend_pullback.rs    trend-pullback-1.0.0 (long only); trend_pullback_short.rs its mirror
    src/breakout.rs, mean_reversion.rs   breakout-1.0.0, mean-reversion-1.0.0
    src/catalog.rs           implementations in the build, matched by logic version + parameters
  crates/qd-app/             use cases + ports (async)
    src/ports.rs             Clock, Journal, HaltStore, BrokerOrderExecutor, BrokerAccountReader, EvidenceSource
    src/orders.rs            OrderIntent, EntryAuthorization, BrokerOrderRequest (crate-private constructors)
    src/decision.rs          Trade Proposal Engine + Decision Engine (journal before authorization)
    src/gateway.rs           Order Gateway: the only order path
    src/positions.rs         Position Manager: OCO protection, exits, reconciliation
    src/live.rs              live gate (INV-14); `live-orders` feature, off by default
    src/memory.rs            in-memory journal and halt store (backtests, tests)
    src/monitor.rs           health, alerts, Prometheus text (ADR 0012)
    src/registry.rs          Strategy Registry (stage = replay of stored events)
    src/session.rs           daily trading cycle shared by backtest and paper (INV-08)
    src/restore.rs           rebuild gateway/positions/book from the journal; fail closed
    src/runs.rs              state, stage slots, instruments, book JSON for paper and live runs
    src/book_view.rs         portfolio view: equity/drawdown, exposure, P&L per strategy
    src/evidence.rs          evidence records (INV-11), evidence tables, stored evidence source
    src/review.rs            predicted-vs-realized review, calibration, paper-review evidence
    src/secrets.rs           catalog of secrets the owner can enter in the web UI
  crates/qd-broker-kite/     Zerodha Kite Connect (ADR 0014, docs/integrations/kite.md)
    src/client.rs, login.rs  HTTP client and error classes; login URL, checksum, token exchange
    src/market.rs            instruments CSV, completed daily candles, IST
    src/broker.rs            executor (orders only with live-orders; OCO = two-leg GTT),
                             account reader, fills from the order book
    src/runner.rs            LiveRunner (latest date only, intraday fill sync), demote_on_breach
  crates/qd-agent/           the AI agent (ADR 0016): tool-calling loop (OpenAI/xAI/Gemini),
                             web research, QuantDesk tools, runs with budgets and traces,
                             prediction scoring, live gate; trades only via AgentDesk
  crates/qd-ai-providers/    OpenAI/xAI (Responses) and Gemini advisors; advisory only (INV-04)
  crates/qd-ai/              advisory AI (INV-04): Advisor, checklist-v1, orchestrator, scorecard;
                             depends on nothing that can act on trading state (tested)
  crates/qd-broker-paper/    PaperBroker (daily-bar fill rules, restorable), PaperRunner
  crates/qd-backtest/        run_backtest (loop over the session), research runner, metrics,
                             validation (walk-forward/OOS/holdout), montecarlo, validator,
                             search (walk-forward parameter search; research only)
  crates/qd-store/           PostgreSQL adapters (journal, halts, market data, registry, audit, accounts)
    src/settings.rs          settings versions; encrypted secrets (XChaCha20-Poly1305)
  crates/qd-server/          config (secrets from env), startup (INV-07), /health, /ready; bin qd-server
    src/runtime.rs           effective settings (files + web UI), per-run services, SettingsAdmin (ADR 0013)
    src/kite.rs              Login with Zerodha, bar import, DynLive, schedules, reconcilers
    src/notify.rs            Telegram notifier
    src/data.rs              instruments and bar CSV uploads with data-quality checks
    src/keys.rs              crash-safe master-key rotation and startup recovery
  crates/qd-cli/             bin `qd`: migrate, accounts, users, instruments, bars, strategy, halts, backtest
  crates/qd-api/             HTTP API under /api (ADR 0008)
    src/auth.rs              argon2id, session cookie, Caller extractor, step-up, login throttle
    src/totp.rs              RFC 6238 TOTP, base32
    src/routes.rs            handlers: parse, authorize, call a port/use case, map
    src/dto.rs               post-risk decision summaries (INV-17)
    openapi.yaml             hand-written API description, served at /api/openapi.yaml
    tests/api.rs             auth, CSRF, step-up, INV-07/14/17 through HTTP
  migrations/                SQL schema; history tables are append-only by trigger (INV-16)
  config/                    data, not code
    calendars/india.toml     NSE/BSE/NFO and MCX holidays (2026, from Zerodha's list)
    costs/india-zerodha.toml cost schedules (verified 2026-09-27 against zerodha.com/charges)
    risk.toml                §4 default risk configuration
    validation.toml          validation protocol and pass criteria (ADR 0010)
    review.toml              paper-review pass criteria (ADR 0010)
    quantdesk.example.toml   server settings (no secrets)
    instruments/examples/    example instrument spec (illustrative terms)
frontend/                    Vite + strict TypeScript, no framework (ADR 0008)
  src/format.ts              decimal strings → display (BigInt, Indian grouping), IST times
  src/labels.ts              §6.3 action labels and NO TRADE reason text
  src/api.ts, dom.ts         API client (CSRF header), textContent-only DOM builder
  src/views.ts, main.ts      screens, top bar (PAPER/LIVE, halted), hash router
  tests/                     vitest
deploy/                      compose stack (postgres, qd-server, Caddy TLS), container config,
                             .env.example (names only), backup.sh (encrypted, off-site),
                             restore-test.sh (ADR 0012, 0015)
Dockerfile                   multi-stage image, non-root, built-in healthcheck
design/stitch-reference/     Stitch export: visual reference only, not requirements
docs/                        spec, ADRs, progress log, OPERATIONS.md, integrations/ (provider notes)
.github/workflows/ci.yml     runs the commands above
```

Target crate not yet created (§5.3): qd-marketdata (Kite provides the bars; NSE is
unreachable: `docs/integrations/`). The crypto venue awaits an owner decision.
Create a crate only when it has real code.

## Rules of thumb

- Decimal everywhere for prices, quantities and money; `f64` only inside statistics.
- Pure crates never read the clock; pass time in (each pure crate's clippy.toml bans it).
- Rates, limits and thresholds are config/data; strategy parameters belong to the logic version.
- Formulas include the contract multiplier and FX (ADR 0003).
- Tick rounding is conservative for the trade; quantities round down.
- Fakes are named `Fake*`/`Mock*`, live in test support or behind `dev-fakes`.
- Secrets are write-only through the API: never add a route or DTO that returns a value;
  only server-side adapters get `SecretReader`. Live trading, environment, account and
  database are never editable from the UI (ADR 0013).
- AI advice is shadow-only: `qd-ai` may depend only on `qd-app` and `qd-domain`, and
  its only write is an `ai_advice` journal entry (ADR 0011).
- The AI agent (ADR 0016) reaches trading only through the `AgentDesk` port (Decision
  Engine → Risk Gate → Position Manager → Order Gateway). Its allocation only ever caps
  the gate's size (`EntryRequest.max_quantity`); never add an agent tool that changes
  limits, halts, settings, keys, strategies or accounts. Web content is untrusted data.
  The live book for the agent is server-file only (`[agent_live]`) and needs its scored
  record; keep `qd-agent` free of broker, store, server, API and risk dependencies.
- Promotions cite recorded evidence that the registry checks: a passed validation up to
  Paper, a passed paper review for live stages (ADR 0010). Never weaken these checks.
- The journal is the source of truth for trading state; never add mutable position or
  order tables. Every Position Manager change journals a snapshot (ADR 0009).
- Only the Decision Engine can create an `EntryAuthorization`; only the Order Gateway
  can create a `BrokerOrderRequest`. Keep those constructors `pub(crate)`.
- Frontend: never `innerHTML`; never parse decimals into JS numbers; dangerous actions
  need step-up on the server, not just a UI prompt.
- No `todo!`, `unimplemented!`, `dbg!`, `unwrap`, `expect` in runtime code.
- Never place a real order. Never ask for secret values; name the `.env` variables.
- Broker adapters are tested against local fake servers only; order placement must stay
  behind `live_orders_compiled()` and the gateway's INV-14 check.
- The live book is configured only in the server file (`[live]`), never in the UI.
- Parameter-search results are research only: never evidence, never registrable; a
  better set becomes a new logic version (INV-10). Keep grids at 12 candidates or fewer.
- Anything encrypted with the master key must be included in `PgSecrets::sealed_rows`
  so key rotation re-encrypts it.
- Read the provider's current docs before any integration; record them in
  `docs/integrations/<provider>.md`.
- Small conventional commits, only when every check passes.

## Invariants (spec §3); tests are named `invariant_NN_*`

- INV-01 One order path: only the Order Gateway can reach order-placing broker calls.
- INV-02 Halts block risk-increasing orders, never exits; exits never exceed open qty.
- INV-03 The Risk Gate can reject any entry; nothing bypasses it, manual entries included.
- INV-04 AI is advisory; it never touches orders, sizes, limits, params, credentials, halts.
  Owner-directed exception (ADR 0016): the AI agent requests entries/exits and allocations,
  always through the Risk Gate and Order Gateway, never limits, params, credentials or halts.
- INV-05 No journal, no trade: decisions and intents are durably written before broker calls.
- INV-06 Fail closed on missing/stale/inconsistent inputs; unknown halt state = halted.
- INV-07 Startup keeps entries halted until reconciliation; hard halts need a human re-arm.
- INV-08 Same strategy/risk/gateway/cost code in backtest, paper and live.
- INV-09 Point-in-time data only; every decision replays from an immutable snapshot.
- INV-10 Strategy versions are immutable; only validated versions at trading stages trade.
  The AI agent's evidence is its scored prediction record instead (ADR 0016).
- INV-11 Promotion needs owner approval with evidence; demotion on breach is automatic.
- INV-12 Actions are OpenLong/CloseLong/OpenShort/CloseShort; no bare BUY/SELL.
- INV-13 Exact decimal arithmetic; levels and quantities rounded before RR/EV/size.
- INV-14 Live orders need the `live-orders` feature, production env, config, `live_armed`, live stage.
- INV-15 Secrets stay on the server; the frontend talks only to the QuantDesk backend.
- INV-16 Journal, order events, halt events, stage transitions and audit logs are append-only.
- INV-17 Every decision display is post-risk; a risk-blocked signal shows NO TRADE + reason.
