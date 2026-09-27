# QuantDesk progress log

## Phase status

The phase plan is provisional until spec §17 is provided (ADR 0001).

| Phase | Scope | Status |
|---|---|---|
| 0 | Audit and plan | Done, compressed (owner directed to start coding) |
| 1 | Domain core `qd-domain` | **Done** (2026-09-27) |
| 2 | `qd-risk`, `qd-strategy`, cost model | **Done** (2026-09-27) |
| 3 | `qd-app` ports and use cases, `qd-backtest` | Next |
| 4 | `qd-store`, `qd-server`, `qd-cli` | Not started |
| 5 | `qd-api` and frontend integration | Not started (needs §10) |
| 6 | Market data adapters, paper broker | Not started (needs §4 items 1, 5, 11) |
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
  seen running on GitHub).

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
  plan, VPS/static IP, DDPI, AI providers and budgets, frontend approach, FX source.
- The branch is pushed (2026-09-27). No CI run had appeared on GitHub right
  after the push; check that Actions is enabled for the repository.
- `trend-pullback-1.0.0` has no evidence yet. Its probabilities will come
  from validation (Phase 7); until then it cannot pass the evidence gate.

## Next steps (Phase 3)

1. `qd-app`: ports (clock, market data, account reader, order executor,
   repositories, journal) and use cases: Trade Proposal Engine (strategy →
   reference-quantity costs → probabilities from evidence tables → proposal),
   Decision Engine (regime, evidence ≥ 30, EV, RR, already-in-position,
   staleness gates → `DecisionOutcome`), Risk Gate orchestration, Order
   Gateway (the only order path, halts, idempotency, journal-before-send),
   Position Manager (protection, exits, reconciliation).
2. `qd-backtest`: simulated clock and broker with the shared fill and cost
   model, running the same use cases (INV-08).
3. In-memory repository fakes for tests only (`Fake*`, test support).
