# QuantDesk progress log

## Phase status

The phase plan is provisional until spec §17 is provided (ADR 0001).

| Phase | Scope | Status |
|---|---|---|
| 0 | Audit and plan | Done, compressed (owner directed to start coding) |
| 1 | Domain core `qd-domain` | **Done** (2026-09-27) |
| 2 | `qd-risk`, `qd-strategy`, cost model | **Done** (2026-09-27) |
| 3 | `qd-app` ports and use cases, `qd-backtest` | **Done** (2026-09-27) |
| 4 | `qd-store`, `qd-server`, `qd-cli` | **Done** (2026-09-27) |
| 5 | `qd-api` and frontend integration | **Done** (2026-09-27) |
| 6 | Market data adapters, paper broker | Next (needs §4 items 1, 5, 11) |
| 7 | Validation pipeline and review reports | Not started |
| 8 | AI orchestrator (advisory) | Not started (needs §4 item 8) |
| 9 | Kite live order executor (`live-orders`) | Not started (needs §4 items 5–7) |

## Phase 0: audit (2026-09-27)

- Repository: empty, no commits, no backend, no frontend app, no CI, no Docker.
- Toolchain available: Rust 1.94.1 (rustfmt, clippy), cargo-deny 0.20.2
  (installed this session), PostgreSQL client, Docker, Node 22.
- Spec: saved verbatim as provided; it ends at §6.6 (§7–§20 missing).
- Frontend: a Stitch export (static HTML, Tailwind Play CDN, four screens,
  logo SVG, `DESIGN.md`), kept as a visual reference in
  `design/stitch-reference/` with its known issues listed there.
- Owner decisions: defaults recorded in ADR 0004; open items listed below.

## Phase 1: domain core (2026-09-27)

Built `backend/crates/qd-domain`: typed UUIDv7 ids; decimal newtypes (Price,
Quantity, Money, Currency, Ratio, Bps, FxRate); versioned `InstrumentSpec` with
tick and quantity rounding; explicit trade actions and UI labels; decision
outcomes and typed no-trade reasons; tick-rounded trade plans; per-unit
economics, outcome probabilities and EV; sizing steps 1, 2, 3 (cap helper) and 5;
open risk, P&L, drawdown and R-multiple; complete trade proposals; halts with
the entry/order check; the exit-quantity rule; strategy, order-intent and
position state machines.

### Verified (all run on 2026-09-27, all passing)

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo test --workspace`: 73 tests (14 unit, 26 formula examples,
  18 invariant, 7 state-machine table, 8 property tests with 256 cases each).
- `cargo deny check`: advisories, bans, licenses, sources all ok.
- The clippy clock ban was checked by adding a `SystemTime::now()` call to
  `qd-domain`: clippy failed as intended, and the probe was removed.
- Mutation check: five deliberate bugs were each caught by the tests
  (stop rounding direction, halts blocking exits, ignoring the multiplier,
  exits above open quantity, quantities rounding up).

Invariant tests so far: `invariant_02_*` (4), `invariant_06_*`,
`invariant_07_*` (2), `invariant_10_*`, `invariant_11_*` (2),
`invariant_12_*` (3), `invariant_13_*` (3), `invariant_17_*`. These cover the
domain half of each invariant; the Gateway, journal, store and API halves get
their tests when those components exist.

## Phase 2: Risk Gate, cost model, first strategy (2026-09-27)

Built:

- `qd-domain/costs.rs`: the `CostModel` port and a schedule-based model;
  schedules are versioned data in `backend/config/costs/india-zerodha.toml`
  (NSE equity delivery, NSE gold/silver ETFs, MCX futures), all marked
  **unverified**.
- `qd-domain/market.rs`: validated daily bars and point-in-time bar series.
- `qd-risk`: validated `RiskConfig` (§4 defaults in `backend/config/risk.toml`),
  the Risk Gate (halts, input consistency, stage, short permission, RR/EV
  gates, daily/weekly/drawdown/cool-off limits, sizing with total/bucket/
  strategy caps and cost re-pricing at the final quantity), halt triggers
  (hard halt on drawdown, cool-off on consecutive losses).
- `qd-strategy`: feature set `features-v1` (SMA 20/50/200, Wilder ATR 14,
  20-day high/low, 20-day rate of change), regime classifier `regime-v1`, the
  `Strategy` trait with `run_strategy`, and `trend-pullback-1.0.0`.
- CI workflow `.github/workflows/ci.yml` running the §1.4 commands (not yet
  seen running on GitHub at the time; it has since run green).

Decisions and assumptions: ADR 0005.

### Verified (all run on 2026-09-27, all passing)

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo test --workspace`: 113 tests (qd-domain 84, qd-risk 19, qd-strategy 10),
  including hand-computed contract-note examples for NSE delivery and an MCX
  short, and an end-to-end test: bars → strategy → proposal → Risk Gate approval.
- `cargo deny check`: advisories, bans, licenses, sources all ok.
- Mutation check: seven deliberate bugs (caps ignored, live accepting
  unverified costs, daily loss check removed, GST base wrong, no resize when
  costs rise, regime ignoring the 200-day slope, strategy ignoring its hold
  rule). The first run caught four; tests were added for the other three and
  all seven are now caught.

New invariant tests: `invariant_02_active_halts_reject_entries`,
`invariant_03_each_limit_can_reject_an_otherwise_good_entry`,
`invariant_06_unknown_or_inconsistent_inputs_fail_closed`,
`invariant_07_drawdown_triggers_a_hard_halt_only_a_human_can_clear`,
`invariant_09_series_refuse_bars_after_the_last_completed_date`,
`invariant_09_evaluations_use_only_bars_up_to_the_decision_date`,
`invariant_10_only_trading_stages_pass`,
`invariant_14_live_needs_a_live_stage_and_verified_costs`.

## Phase 3: application layer and backtesting (2026-09-27)

Built:

- `qd-app`: ports (clock, journal, halt store, broker order executor and
  account reader, evidence source); order intents, entry authorizations and
  broker order requests (crate-private constructors enforce INV-01/INV-03);
  the Decision Engine with the Trade Proposal Engine; the Order Gateway
  (idempotency, validation, halts, exit-quantity ledger with OCO, live gate,
  journal-before-send with an entry-halt latch, unknown-state handling,
  fills, cancels, expiry, reconciliation of unknown intents); the Position
  Manager (OCO protection, time exit, invalidation, flatten, reconciliation);
  the live gate; in-memory journal and halt store.
- `qd-backtest`: `SimClock`, `SimBroker` with conservative daily-bar fill
  rules (`simulate_fill`), `run_backtest` running the shared use cases, trade
  records and metrics (win rate, expectancy in R, profit factor, max
  drawdown, total return, Sharpe).

Decisions and assumptions: ADR 0006.

### Verified (2026-09-27, all passing)

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
  (includes the `live-orders` feature)
- `cargo test --workspace`: 138 tests; `cargo test -p qd-app --features live-orders` also passes.
- `cargo deny check`: all ok.
- Mutation check: six deliberate bugs (halt check skipped, journal failure
  ignored, committed exits ignored, authorization quantity unchecked, OCO
  letting both orders fill, evidence minimum ignored). Five were caught at
  once; a same-bar stop-and-target test was added for the OCO case and now
  catches it. The authorization-quantity check cannot be reached through
  the public API (the intent copies the approved quantity); it stays as a backstop.

New invariant tests: `invariant_02_an_active_halt_blocks_the_entry_at_the_gateway`,
`invariant_02_exits_pass_halts_but_never_exceed_the_open_quantity`,
`invariant_03_a_risk_rejection_releases_no_authorization`,
`invariant_05_no_journal_no_authorization`,
`invariant_05_the_intent_is_journaled_before_the_broker_call`,
`invariant_05_a_journal_failure_sends_nothing_and_halts_entries`,
`invariant_06_an_unreadable_halt_store_blocks_entries`,
`invariant_14_live_orders_are_blocked_by_default`,
`invariant_14_every_live_condition_is_required`.

## Phase 4: persistence, server, CLI (2026-09-27)

Built:

- `backend/migrations`: schema with database-enforced append-only history
  (INV-16), point-in-time bars, versioned instrument specs, immutable strategy
  versions and stage events, audited accounts.
- `qd-store`: PostgreSQL journal, halt store (re-validated on read), market
  data (instruments, point-in-time bars), strategy registry store, audit log,
  accounts (live arming audited, impossible on non-live accounts).
- `qd-app/registry.rs`: the Strategy Registry use case (stage = replay of
  stored events through the state machine).
- `qd-domain`: halts can be read back from storage through their validating
  constructors (`HaltRecord`).
- `qd-server`: validated configuration (secrets from the environment only,
  redacted), safe defaults, refusal to start with unsafe live settings, the
  conservative startup sequence, `/health` and `/ready`.
- `qd-cli` (`qd`): migrate, accounts, instruments, bar import, strategy
  registry, halts, journal, research backtest.
- CI: PostgreSQL service; a second test run with `live-orders` compiled in.

Decisions and assumptions: ADR 0007.

### Verified (2026-09-27, all passing)

- fmt, clippy (all features), `cargo deny check` (BSD-3-Clause and Zlib added
  to the license allow-list for sqlx/axum dependencies).
- `cargo test --workspace` with `DATABASE_URL` set: 150 tests; with
  `live-orders`: qd-app and qd-server 23 tests.
- End-to-end smoke run on a fresh local PostgreSQL 16: `qd migrate`, account,
  instrument, 600-bar CSV import, strategy registration and stage events (an
  illegal transition was refused), manual halt, research backtest (18 trades
  on synthetic data), then `qd-server`: `/health` ok, `/ready` showed entries
  halted by the startup and manual halts; no secret in the logs.

New invariant tests: `invariant_16_history_tables_reject_updates_deletes_and_truncates`,
`invariant_07_halts_survive_in_the_store_and_are_revalidated`,
`invariant_07_stored_halts_are_revalidated_when_read_back`,
`invariant_09_bar_corrections_are_invisible_to_earlier_snapshots`,
`invariant_10_versions_are_immutable_and_stages_replay`,
`invariant_14_only_live_accounts_can_be_armed_and_arming_is_audited`,
`invariant_15_the_database_url_comes_from_the_environment_and_is_redacted`,
`invariant_14_live_trading_outside_production_or_with_unverified_costs_refuses_to_start`,
`invariant_07_without_a_broker_entries_stay_halted_after_startup`,
`invariant_07_startup_clears_only_after_clean_reconciliation`.

## Phase 5: HTTP API, authentication, frontend (2026-09-27)

Built:

- `qd-api`: the HTTP API under `/api`: login, logout, current user and
  step-up; status (mode, halts, entries halted); decisions (post-risk
  summaries, INV-17), journal, halts (create, step-up re-arm), strategy
  registry and stage events (step-up for promotions and resumes),
  instruments and bars, research backtests, live arming (step-up, live
  accounts only), SSE journal events, hand-written OpenAPI.
- Security: argon2id passwords, hashed session tokens in `HttpOnly`
  `SameSite=Strict` cookies, CSRF header check, login throttling, owner and
  viewer roles, audited actions, strict security headers.
- `migrations/..._auth.sql`: users (one owner at most) and sessions.
- `qd user create` (password on stdin); `qd-server` serves the API and, with
  `frontend_dir`, the built frontend.
- `qd-backtest/research.rs`: `ResearchBacktester`, the backtest port used by
  the API and the CLI.
- `frontend/`: Vite and strict TypeScript: login, decisions list and detail,
  halts, strategies, backtest, journal; PAPER/LIVE and halt badges; decimal
  formatting without floats; IST times; no `innerHTML`.
- CI: a frontend job (npm ci, typecheck, tests, build).

Decisions and assumptions: ADR 0008.

### Verified (2026-09-27, all passing)

- fmt, clippy (all features), `cargo deny check`.
- `cargo test --workspace` with `DATABASE_URL`: 157 tests, including 7 API
  tests through the full router against PostgreSQL.
- Frontend: `npm run typecheck`, `npm test` (7 tests), `npm run build`,
  `npm audit` 0 vulnerabilities (vitest upgraded to 5 for a dev-only advisory).
- Smoke run: `qd migrate`, `qd user create`, `qd-server` with the built
  frontend: `/` and a client route served `index.html`; `/api/auth/me`
  returned 401 before login; login set the cookie; `/api/status` showed
  paper mode and entries halted; the CSP and frame headers were present; the
  server log contained no password.

New invariant tests: `invariant_07_rearming_a_halt_needs_a_step_up`,
`invariant_14_arming_needs_step_up_and_a_live_account`,
`invariant_17_a_risk_blocked_decision_is_shown_as_no_trade_with_its_reason`.

## Open issues

- Spec §7–§20 missing from `docs/QUANTDESK_BUILD_SPEC.md`. Phase 2 used only
  the §4 defaults for risk limits (§7.8 unknown). Still needed: §7.3 scanner
  records, §7.5 AI policy, §10 frontend, §17 phase exit criteria, §20 report format.
- §6.5 formulas omit the contract multiplier and FX; implemented with them
  (ADR 0003). The spec text should be updated.
- **Cost schedules are unverified** (zerodha.com unreachable from the build
  environment). The owner must check every rate in
  `backend/config/costs/india-zerodha.toml` against https://zerodha.com/charges
  and mark schedules verified. Live accounts refuse unverified schedules.
- Assumptions to confirm (ADR 0005): cool-off lasts 24 hours; gap shocks
  equity 20%, precious metals 10%, energy 20%, crypto 30%.
- Owner decisions still open (ADR 0004): crypto venue, stock universe, Kite
  plan, VPS/static IP, DDPI, AI providers and budgets, FX source.
- CI runs on GitHub for every push and passed for Phase 2.
- Backtests over dates before 2026-01-01 fail closed until historical cost
  schedule versions exist; backtests support one currency only (ADR 0006).
- The gateway's journal-failure latch clears only on restart (ADR 0006).
- Tests need a PostgreSQL reachable through `DATABASE_URL` (ADR 0007).
- Redis is deferred until a concrete need (ADR 0007).
- Positions are not yet persisted: the server's book starts empty, so any
  broker position at startup is a reconciliation mismatch (Phase 6).
- `trend-pullback-1.0.0` has no evidence yet. Its probabilities will come
  from validation (Phase 7); until then it cannot pass the evidence gate.

- Login throttling is in memory and resets on restart (ADR 0008).
- A startup halt from a failed boot stays active until the owner re-arms it
  (ADR 0008).

## Next steps (Phase 6)

1. Persist positions and orders so reconciliation compares the stored book
   with the broker.
2. `qd-broker-paper`: a paper broker for live market data, using the same
   fill rules as the backtest (INV-08).
3. `qd-marketdata`: daily-bar ingestion adapters (read provider docs first;
   record them in `docs/integrations/`).
4. The scanner and daily run loop: after each close, run the Decision Engine
   over the universe, then the Gateway and Position Manager in paper mode.
5. Manual entries through the Decision Engine and Risk Gate (INV-03), and
   positions and orders views in the frontend.
