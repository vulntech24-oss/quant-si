# ADR 0009: Phase 6 decisions: paper trading, restore from the journal

- Status: accepted
- Date: 2026-09-27

## Context

Phase 6 was planned as market-data adapters, the paper broker, the Kite
adapter, persisted positions, the daily run loop and manual entries. The
spec's sections on these (§7 onwards) are missing. The provider docs for Kite,
Binance and NSE could not be fetched from the build environment (egress
policy), and the project rules forbid writing an integration without reading
the provider's current docs.

## Decisions

### Scope actually built

- **Built:** state restored from the journal; the shared daily cycle; the
  paper venue (`qd-broker-paper`); the paper runner (CLI, API, optional daily
  schedule); startup reconciliation against the paper venue; paper views in
  the frontend.
- **Not built:** Kite and crypto adapters, because their docs were
  unreachable. They are recorded in `docs/integrations/` with what they will
  need. Daily bars keep coming in through `qd bars import`. Manual entries are
  deferred to the phase that adds the scanner UI. They will go through the
  Decision Engine and Risk Gate (INV-03).

### One daily cycle for backtest and paper (INV-08)

- The per-day logic moved from `qd-backtest/engine.rs` into
  `qd-app/session.rs` (`TradingSession::process_day`). The backtest engine is
  now a loop over dates. Paper trading runs the same function.
- The simulated venue (`SimBroker`, now `qd_broker_paper::PaperBroker`)
  moved into `qd-broker-paper`; `qd-backtest::sim` re-exports it under the
  old names. The fill rules are unchanged (ADR 0006). The backtest produced
  identical results before and after the move; only trade ids differ.
- Each day ends with a `day_closed` journal entry that holds the account book
  (equity, high-water mark, week start, consecutive losses) and the day's
  closed trades.

### Persistence: the journal is the source of truth

No mutable position or order tables were added. Everything is rebuilt from
the append-only journal (INV-05, INV-16):

- Order intents, order events and fills rebuild the Order Gateway and its
  open-quantity ledger.
- Every Position Manager change journals a full `position_snapshot`, and the
  latest snapshot per position rebuilds the book.
- The last `day_closed` restores the account book. The trades in all
  `day_closed` entries mark which closed positions are already booked.
- Restore rules on the safe side:
  - An intent journaled but never accepted was never sent.
  - An accepted intent without a journaled broker answer is `Unknown`. For
    the paper venue it is reconciled as never having reached the venue.
  - Fill state follows the journaled fills.
- `RestoredState::check` refuses a book that its order ledger contradicts,
  per position and per instrument and side. A refused restore records an
  account-scoped **operational halt that needs a human re-arm**, and the run
  stops (INV-06). The one crash window that can cause this is a fill
  journaled but its snapshot lost. A human then decides what happened.
  Nothing auto-repairs the book.
- Order-intent deserialization stays crate-private in `qd-app`, so JSON
  cannot become a way to build an authorized entry (INV-01, INV-03).
  Restoring sends nothing to any venue.
- The journal holds every account; restore filters by account.

### Two clocks

- The **market clock** is set to each date's close. It stamps order
  intents, so the venue knows which bar an order was placed on.
- The **wall clock** decides whether halts are active and stamps decisions
  and gateway events.
- In a backtest both are the same simulated clock. In paper trading the wall
  clock is real, so **a halt set today also blocks a catch-up run over
  earlier dates**. Without this, a past-dated decision would not see a halt
  created later. A test covers it, and the mutation that uses the market
  clock for decisions fails it.

### The paper runner

- `qd paper run --config <server config> [--through DATE]`,
  `POST /api/paper/run {through}` (owner; future dates refused), or the
  optional daily schedule (`[paper] daily_run_utc`).
- One run takes a PostgreSQL advisory lock per account (`PgRunLock`), so the
  CLI and the server cannot process the same day at once. It then:
  1. refuses non-paper accounts;
  2. restores and checks the book;
  3. processes every date with bars after the last `day_closed`, up to
     `through`.
  A second run over the same dates does nothing.
- **Strategies:** only versions at the Paper stage whose logic version exists
  in the binary and whose registered parameters equal the code's parameters
  (INV-10). Others are reported as skipped.
- **Evidence:** paper decisions use `EvidencePolicy::Required` with
  `min_evidence` from the risk configuration. There are no evidence tables
  until Phase 7 (`NoEvidenceTables`), so **every paper decision is NO TRADE
  with `insufficient_evidence`** until validation produces evidence. This is
  intended and fails closed.
- **Data:** instruments are the latest spec versions effective at `through`,
  and bars are as known at the start of the run (INV-09). Futures trade as
  Margin, everything else as Delivery.
- **Configuration** (`[paper]` in the server config):
  - `initial_equity` and `start_date` are required;
  - `close_time_utc` defaults to 10:00, the NSE close at 15:30 IST;
  - `slippage_ticks` defaults to 1;
  - `warm_up_days` defaults to 400;
  - `daily_run_utc` is optional; without it, runs are manual only.
  Without a `[paper]` section there is no runner, and entries stay halted at
  startup.

### Startup (INV-07)

`startup()` now takes a `Reconciler` port instead of a broker account reader.
With paper configured, reconciliation restores the book, checks it, and
compares the book with the venue's net positions. A clean result clears the
startup halt. Any problem keeps entries halted and is logged.

## Consequences and how to change

- Restore reads every state entry of the journal on each run. That is fine
  for daily bars and one owner. If it becomes slow, add periodic
  `day_closed` checkpoints that carry positions and working intents, and
  replay only from the last one.
- Paper reconciliation compares the book with a venue rebuilt from the same
  journal, so it finds journal inconsistencies, not venue drift. A live
  broker (Phase 9) will reconcile against the broker's own positions and orders.
- Costs that the cost model cannot quote book as zero, as the backtest
  always did. Revisit this with verified schedules.
