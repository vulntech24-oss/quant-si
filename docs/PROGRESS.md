# QuantDesk progress log

## Phase status

The phase plan is provisional until spec §17 is provided (ADR 0001).

| Phase | Scope | Status |
|---|---|---|
| 0 | Audit and plan | Done, compressed (owner directed to start coding) |
| 1 | Domain core `qd-domain` | **Done** (2026-09-27) |
| 2 | `qd-risk`, `qd-strategy`, cost model | Next |
| 3 | `qd-app` ports and use cases, `qd-backtest` | Not started |
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

## Open issues

- Spec §7–§20 missing from `docs/QUANTDESK_BUILD_SPEC.md`. Needed before
  Phase 2 risk limits (§7.8), AI policy (§7.5), frontend (§10), phase exit
  criteria (§17) and report format (§20).
- §6.5 formulas omit the contract multiplier and FX; implemented with them
  (ADR 0003). The spec text should be updated.
- Owner decisions still open (ADR 0004): crypto venue, stock universe, Kite
  plan, VPS/static IP, DDPI, AI providers and budgets, frontend approach, FX source.
- No CI yet. Proposed with Phase 2: a GitHub Actions workflow running the
  §1.4 commands.

## Next steps (Phase 2)

1. `qd-risk`: Risk Gate rules from the §4 default configuration (per-trade
   risk, total/bucket/strategy open-risk caps, daily/weekly loss, drawdown
   hard halt, consecutive-loss cool-off, stage multipliers), cost re-check at
   the final quantity (sizing step 4), halt triggers.
2. Versioned cost model for NSE delivery equity, ETFs and MCX futures, with
   rates as dated data. Rates must come from current official schedules.
3. `qd-strategy`: feature engine on completed daily bars, deterministic
   regime classifier, `Strategy` trait, one strategy producing complete proposals.
