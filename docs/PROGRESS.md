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
| 6 | Paper trading, restore from the journal (provider adapters blocked) | **Done** (2026-09-27) |
| 7 | Validation, evidence, Monte Carlo, review and calibration | **Done** (2026-09-27) |
| 8 | Advisory AI in shadow mode (provider adapters blocked) | **Done** (2026-09-27) |
| 9 | Deployment, monitoring, alerts, operator docs (live broker blocked) | **Done** (2026-09-27) |
| — | Zerodha Kite, live runner, hosted AI advisors, Telegram, verified costs | **Done** (2026-09-27, ADR 0014) |
| — | Upgrades: calendars and data quality, Data page, 3 strategies, portfolio and charts, TOTP and sessions, settings history, key rotation, backups, parameter search, mobile | **Done** (2026-09-27, ADR 0015) |

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

## Phase 6: paper trading and restore from the journal (2026-09-27)

Built:

- `qd-app/session.rs`: the daily trading cycle, shared by backtest and paper
  (INV-08). It covers bars and fills, exits, booking closed trades, marking to
  market, halt triggers, reconciliation and decisions for every strategy
  slot, and ends with a `day_closed` journal entry. It uses two clocks: the
  market clock for order placement, and the wall clock for halts and decisions.
- `qd-app/restore.rs`: rebuilds the gateway, positions and account book from
  the journal and refuses inconsistent books. The Position Manager now
  journals full snapshots, the gateway and manager can be restored, and
  held-bar counting is idempotent.
- `qd-broker-paper`: the paper venue (moved from `qd-backtest::sim`,
  restorable) and the paper runner (run lock, account check, restore,
  registry and parameter check, catch-up over missing days).
- `qd-store`: `JournalReader::replay` and `PgRunLock` (advisory locks).
- `qd-server`: `[paper]` configuration, paper reconciliation at startup,
  optional daily schedule. `qd paper run|state`; `GET /api/paper`,
  `POST /api/paper/run`; a Paper screen in the frontend.
- `docs/integrations/`: Kite and crypto recorded as blocked (docs
  unreachable); the CSV bar import documented.

Decisions and assumptions: ADR 0009.

### Verified (2026-09-27, all passing)

- fmt, clippy (all features), `cargo deny check`; frontend typecheck, tests, build.
- `cargo test --workspace`: 166 tests; the `live-orders` run passes.
- The backtest's results are unchanged by the refactor (the determinism
  test compares everything except random ids).
- Key new test: a paper account run straight through and one run in three
  separate runs with restores in between produce identical `day_closed`
  records (dates, equities, trades), with zero reconciliation mismatches.
  Re-running processes nothing.
- Mutation checks:
  - Restoring filled intents as still working (a bug found by a new test and
    fixed) is caught.
  - Using the market clock for decisions is caught by the catch-up halt test.
- Smoke run on local PostgreSQL:
  - `qd paper run` twice: the second run resumed after the first. Every
    decision was NO TRADE with `insufficient_evidence`, as intended.
  - `qd paper state`.
  - `qd-server` reconciled against the paper venue and cleared its startup
    halt. `/api/paper` and `/api/paper/run` worked, and a future date was refused.

New invariant tests: `invariant_08_a_restarted_paper_run_matches_an_uninterrupted_one`,
`invariant_06_a_book_the_orders_contradict_is_refused_and_halts`,
`invariant_02_a_halt_set_now_blocks_entries_in_a_catch_up_run`,
`invariant_06_paper_decisions_without_evidence_are_no_trade`,
`invariant_10_a_version_whose_parameters_differ_from_the_code_is_not_run`,
`invariant_05_the_journal_alone_rebuilds_the_book_and_the_order_ledger`,
`invariant_06_restore_keeps_unanswered_orders_unknown_and_refuses_lost_snapshots`.

## Phase 7: validation, evidence, Monte Carlo, review (2026-09-27)

Built:

- `qd-backtest::validation`: walk-forward out-of-sample windows, a holdout
  run once at the end, evidence tables from out-of-sample trades only, and
  checks recorded with their values.
- `qd-backtest::montecarlo`: seeded bootstrap of R-multiples (drawdown and
  return percentiles, share of paths reaching the hard-halt drawdown).
- `qd-backtest::validator`: validation of registered versions over stored
  bars. Evidence is recorded, pass or fail, and audited.
- `qd-app::evidence`: evidence records, `EvidenceStore`, conservative
  evidence tables, and `StoredEvidence` / `StoreEvidenceLoader` so paper
  decisions use the latest passed validation.
- The registry checks cited evidence:
  - validation for research and Paper;
  - a paper review for live stages.
- `qd-app::review`: predicted-vs-realized review (shares, Brier score,
  calibration bins, EV vs realized R, drawdown, operational incidents),
  paper-review criteria, and recorded paper reviews.
- Setup type and decision id now travel on positions and trades.
- `qd-strategy::catalog`: the implementations in the build, matched to
  registry versions by logic version and parameters.
- `migrations/..._evidence.sql` (append-only), `config/validation.toml`,
  `config/review.toml`.
- API: `POST /api/validations`, `GET /api/evidence[/{id}]`, `GET /api/review`,
  `POST /api/review/{version}/record`. CLI: `qd strategy validate|review|evidence`.
  Frontend: Validation and Review screens; promotions cite recorded evidence.

Decisions and assumptions: ADR 0010.

### Verified (2026-09-27, all passing)

- fmt, clippy (all features), `cargo deny check`; frontend typecheck, tests, build.
- `cargo test --workspace`: 177 tests; the `live-orders` run passes.
- End-to-end test on PostgreSQL:
  - a failed validation is recorded and cannot back a stage event;
  - a passed one promotes to Paper but not to a live stage;
  - paper trading then enters trades using the stored evidence tables;
  - a failed paper review cannot promote to SmallCapital, and a passed one can.
- Mutation check: removing the "evidence must have passed" check fails the
  end-to-end test.
- Smoke run: `qd strategy validate` on the smoke database recorded a failed
  validation with every reason (too little history after warm-up), and
  `qd strategy review` ran.

New invariant tests: `invariant_11_stage_events_need_recorded_passing_evidence_of_the_right_kind`,
`invariant_11_validation_evidence_promotes_and_feeds_paper_decisions`.

## Phase 8: advisory AI in shadow mode (2026-09-27)

Built:

- `qd-ai`:
  - the `Advisor` interface and an input packet built from the post-risk
    decision (no equity, quantities or secrets);
  - validated, bounded advice;
  - an orchestrator: entry decisions only, once per advisor, daily budget,
    timeout per call, failures isolated;
  - `checklist-v1`, a deterministic advisor;
  - a scorecard of stances against outcomes;
  - `AiService` behind the `AiAdvisory` port.
- `JournalEntry::AiAdvice` (append-only): the orchestrator's only write.
- `qd-server`: `[ai]` config (disabled by default), wiring, and an AI run
  after the daily paper run. `qd ai run|advice|scorecard`; `POST /api/ai/run`,
  `GET /api/ai/advice`, `GET /api/ai/scorecard`. Frontend: an AI page, and
  advice on the decision detail marked advisory.
- `docs/integrations/ai-providers.md`: provider adapters blocked (docs unreachable).

Decisions and assumptions: ADR 0011.

### Verified (2026-09-27, all passing)

- fmt, clippy (all features), `cargo deny check`; frontend typecheck, tests, build.
- `cargo test --workspace`: 183 tests; the `live-orders` run passes.
- INV-04 tests:
  - the `qd-ai` manifest and sources cannot reach orders, limits, halts, the
    registry, accounts or secrets;
  - the orchestrator appends only advice;
  - on PostgreSQL, an AI run after paper trading leaves every other journal
    entry, halt, stage event and account unchanged.
- Mutation check: removing the daily budget check is caught.

New invariant tests: `invariant_04_the_ai_crate_cannot_reach_orders_limits_halts_or_credentials`,
`invariant_04_the_orchestrator_only_appends_advice_once_per_advisor`
(and the AI part of `invariant_11_validation_evidence_promotes_and_feeds_paper_decisions`).

## Phase 9: deployment, monitoring, alerts, operator docs (2026-09-27)

Built:

- `Dockerfile`:
  - multi-stage;
  - the runtime has no package-manager step;
  - runs as a non-root UID;
  - built-in healthcheck;
  - supports a TLS-intercepting proxy at build time via a BuildKit secret.
- `deploy/`:
  - compose stack: PostgreSQL, a read-only qd-server, and Caddy (TLS, HSTS,
    `/metrics` not served publicly);
  - container configuration, `.env.example` (names only), `backup.sh`.
- `qd-app::monitor`: health, alerts (critical and warning) and Prometheus
  text. `/metrics`, alerts in `/api/status` (a UI banner), and alert-change
  JSON logs.
- Startup reconciliation fails closed unless the configured account exists
  and is a paper account. `qd account create --id`.
- CI: a container build job with a non-root and no-credential check.
- `README.md`, `docs/OPERATIONS.md` (operator guide).

Decisions and assumptions: ADR 0012.

### Verified (2026-09-27, all passing)

- fmt, clippy (all features), `cargo deny check`; frontend typecheck, tests, build.
- `cargo test --workspace`: 186 tests; the `live-orders` run passes.
- Docker image built (146 MB, UID 10001). Compose stack up with TLS:
  - healthy; startup refused while the account was missing;
  - passed after `qd account create --id` and `qd user create`;
  - HTTPS login with a `Secure; HttpOnly; SameSite=Strict` cookie; HSTS and
    CSP present; `/metrics` 404 through the proxy;
  - no password in logs.
  The stack was removed afterwards.

New invariant tests: `invariant_06_an_unreadable_kill_switch_is_a_critical_alert_and_halts`
(and the account checks in `paper_runs_refuse_live_accounts_and_concurrent_runs`).

## Settings page and encrypted secrets (2026-09-27)

Built at the owner's request: every setting and API key is editable in the
web UI, and no TOML editing is needed.

- `qd-server::runtime`: files are the defaults and UI saves override them.
  Paper, risk, AI, validation and review settings are read on every run (no
  restart). Saves are validated, need a step-up, and are audited; history is
  append-only.
- Encrypted, write-only secrets:
  - catalog: Kite key, secret and access token; OpenAI, Gemini and xAI keys;
    crypto key and secret;
  - XChaCha20-Poly1305 with the master key from `QD_MASTER_KEY` or generated
    in `data_dir` (0600);
  - no API route returns a value.
- Live trading, the environment, the account and the database stay
  server-only (INV-14).
- The advisory-AI budget now counts today's journaled advice.
- Frontend **Settings** page; compose volume `qdstate` for the master key.

Decisions: ADR 0013.

### Verified (2026-09-27, all passing)

- fmt, clippy (all features), `cargo deny check`; frontend typecheck, 9
  tests, build.
- `cargo test --workspace`: 192 tests; the `live-orders` run passes. New:
  - `invariant_15_secrets_are_write_only_encrypted_and_audited_by_name`;
  - settings step-up/CSRF/role tests;
  - runtime tests: override, audit, reset, refusal of invalid values (a
    non-zero paper stage multiplier included), append-only history, paper
    on/off, schedule, master-key file 0600 and reuse.
- Smoke run against a real server:
  - master key generated with mode 0600;
  - enabling AI in Settings allowed an AI run immediately;
  - a saved Kite key was never returned by the API and never logged.

## Zerodha Kite, live trading, AI providers, Telegram (2026-09-27)

Built once the owner allowed network access. Every provider's docs were
read first and recorded in `docs/integrations/`.

- **Costs**: all three Zerodha schedules were verified against
  zerodha.com/charges and marked verified.
- **Gateway**: `submit_oco` journals the stop and the target, then sends
  them as one broker call. If the pair is refused, the stop is sent alone.
  Restored intents keep their broker order ids.
- **`qd-broker-kite`**:
  - client;
  - login (checksum, one-time state, client-id check);
  - instruments CSV and completed daily candles;
  - account reader;
  - executor: regular/AMO orders and protective two-leg GTTs, placement
    only with `live-orders`;
  - fills read back from the order book.
- **Live runner**:
  - daily cycle for the latest completed date only;
  - intraday fill checks;
  - unknown orders resolved by tag;
  - automatic demotion to Paper while a hard halt covers a live version
    (INV-11);
  - an operational halt when the book disagrees with Zerodha (INV-06).
- **`qd-ai-providers`**: OpenAI and xAI (Responses API) and Gemini
  (`generateContent`), all with a strict JSON schema, advisory only.
- **Server**:
  - Settings sections `kite` and `notifications`, and per-provider AI
    switches; `[live]` stays file-only;
  - "Login with Zerodha" with a cookie-less callback;
  - scheduled bar import, live run and fill checks;
  - Telegram alerts and summaries;
  - startup reconciliation of the live book;
  - live alerts and metrics.
- **Frontend**: a **Broker** page with:
  - the setup checklist, "Login with Zerodha" and bar import;
  - the Telegram test;
  - the INV-14 conditions, arm/disarm, fill check and live run;
  - the live book.

### Verified (2026-09-27, all passing)

- fmt, clippy (all features), `cargo deny check` (ISC allowed for the
  rustls stack); frontend typecheck, 12 tests, build.
- `cargo test --workspace`: 218 tests. The `live-orders` run (qd-app,
  qd-server, qd-broker-kite) passes. New tests include:
  - `invariant_14_without_every_live_condition_no_order_reaches_kite`;
  - `invariant_11_a_hard_halt_demotes_live_versions_to_paper`;
  - `invariant_06_a_book_that_disagrees_with_kite_halts_live_entries`;
  - with `live-orders`: the AMO entry form, then its fill, gives one
    two-leg GTT with the right triggers and legs;
  - login tests: forged or reused state, wrong client id (token not
    stored), token never shown;
  - Telegram routing and severity;
  - provider request and response shapes, refusals and errors (keys never
    in errors);
  - `invariant_04` for the providers crate.
- Every test runs against local fake servers. No real order was placed, and
  no live endpoint was called.
- Smoke run against a real server:
  - Kite settings and keys saved from the API;
  - the login URL carried a one-time state;
  - a forged callback was redirected to `login=failed`;
  - the secrets never appeared in the log.

## Upgrades (2026-09-27, ADR 0015)

- **Calendars and data quality:** NSE/BSE/MCX 2026 holiday calendars from
  Zerodha's list, with gap and jump checks on every import. Suspect Kite
  bars are held back, CSV jumps need "accept jumps", and the stale alert
  counts trading days.
- **Data page:** add instrument specs (TOML), upload bar CSVs, check stored
  bars.
- **Strategies:** `breakout-1.0.0`, `mean-reversion-1.0.0` and
  `trend-pullback-short-1.0.0`, with a generic register command and
  research backtests for any catalog version.
- **Portfolio page:** equity and drawdown, exposure by bucket and asset
  class, P&L per strategy. Decision pages have a price chart with entry,
  stop and target.
- **Security:**
  - TOTP login (RFC 6238, encrypted secret, replay-proof), plus
    `qd user reset-totp`;
  - session list, revoke, log out everywhere;
  - settings history with compare and restore;
  - crash-safe master-key rotation.
- **Operations:** encrypted off-site backups (rclone), a monthly restore
  test script, Telegram messages for live fills.
- **Parameter search:** walk-forward over grids of at most 12 candidates,
  compared with training and with the catalog parameters. Research only.
- **Mobile:** menu toggle and a bottom bar with the Kill switch.

### Verified (2026-09-27, all passing)

- fmt, clippy (all features), `cargo deny check`; frontend typecheck, 16
  tests, build.
- New tests include:
  - calendar coverage, gaps, holiday bars and jumps; the Kite import
    holding back a suspect bar; the CSV import refusing a jump; staleness
    by trading days;
  - the new strategies' plans and regimes, and the catalog;
  - the portfolio view over a paper run;
  - TOTP against the RFC vectors, login with a code and replay refusal,
    sessions and log out everywhere;
  - settings restore; key rotation including recovery after a simulated
    crash;
  - the search's training-only selection and determinism;
  - chart geometry and the settings diff.
- Phone layout checked in Chromium at 390×844 (Playwright screenshots).

## Open issues

- Spec §7–§20 missing from `docs/QUANTDESK_BUILD_SPEC.md`. Phase 2 used only
  the §4 defaults for risk limits (§7.8 unknown). Still needed: §7.3 scanner
  records, §7.5 AI policy, §10 frontend, §17 phase exit criteria, §20 report format.
- §6.5 formulas omit the contract multiplier and FX; implemented with them
  (ADR 0003). The spec text should be updated.
- Cost schedules were verified against https://zerodha.com/charges on
  2026-09-27 (DP charge ₹15.34 per scrip-day sell; ETF STT not modelled).
  Re-check when Zerodha or the exchanges change rates.
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
- The Kite adapter is tested only against a fake of the documented API.
  Before real money, place one tiny order by hand and check that the
  journal, the book and Kite agree (ADR 0014).
- A GTT stop leg that triggers after a gap beyond its limit stays an open
  LIMIT order at Zerodha; QuantDesk does not chase it (ADR 0014).
- NSE (bot protection) and Binance (HTTP 451 geo-block) remain unreachable
  from the build environment. Kite provides the NSE, BSE and MCX bars.
- Paper decisions are NO TRADE until a version passes validation (ADR 0010).
- Validation and review thresholds are conservative starting points
  (`config/validation.toml`, `config/review.toml`); the owner should review them.
- Costs the cost model cannot quote are booked as zero in backtest and paper
  (ADR 0009); revisit with verified schedules.
- Manual entries (through the Decision Engine and Risk Gate) are not built yet.
- `trend-pullback-1.0.0` has no evidence on real data yet: it needs real
  bars (CSV import) and a passed validation.

- Login throttling is in memory and resets on restart (ADR 0008).
- A startup halt from a failed boot stays active until the owner re-arms it
  (ADR 0008).

## What remains (blocked or owner decisions)

0. **2027 holiday lists:** add them to `config/calendars/india.toml` when
   NSE and MCX publish them. Until then, 2027 dates are "unknown" and the
   checks fall back to calendar days.
1. **Crypto venue**: Binance refuses this region (HTTP 451). The owner
   chooses a venue that serves their country; its adapter comes after its
   docs are read.
2. **Owner actions at Zerodha** before live orders:
   - a Kite Connect plan ("Connect", for historical data);
   - the redirect URL `https://<domain>/api/kite/callback`;
   - a static IP registered for orders;
   - DDPI or TPIN;
   - TOTP.
3. **Going live** is a server decision (INV-14):
   - a `live-orders` build;
   - `environment = "production"`;
   - `live_trading_enabled = true`;
   - a `[live]` book;
   - arming;
   - a version with a passed paper review at SmallCapital.
4. **Missing spec sections §7–§20**: every assumption is recorded in the
   ADRs; the owner should confirm them.
