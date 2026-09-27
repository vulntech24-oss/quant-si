# ADR 0006: Phase 3 decisions: application layer and backtesting

- Status: accepted
- Date: 2026-09-27

## Context

Phase 3 builds `qd-app` (ports, Decision Engine, Order Gateway, Position
Manager) and `qd-backtest`. Spec §7 (component details) is still missing, so
the decisions below fill the gaps. Each keeps the safest behavior as the default.

## Decisions

### One order path and Risk Gate authority, enforced by types (INV-01, INV-03)

- `BrokerOrderRequest` has only a crate-private constructor used by the
  Order Gateway. Broker adapters can read requests but nothing outside
  `qd-app` can build one, so nothing else can place an order.
- `EntryAuthorization` has only a crate-private constructor used by the
  Decision Engine, after the Risk Gate approved and the decision was
  journaled. `OrderIntent::entry` needs one and copies its quantity, so an
  entry cannot exceed or bypass its approval. The gateway re-checks the
  quantity as a backstop (unreachable through the public API).
- Manual entries (Phase 5) must go through the same Decision Engine path.

### Order Gateway

- Keeps its own ledger of filled open quantity per instrument and side.
  A risk-reducing order may not exceed the open quantity minus other working
  reducing orders; orders of one OCO group count once.
- Journal-before-send: the intent and its acceptance are written before the
  broker call. If either write fails, nothing is sent and a latch halts new
  entries for the life of the gateway. **Default: the latch clears only on
  restart**, when startup halts and reconciliation apply (INV-07). Exits are
  never blocked by it.
- An unreadable halt store counts as halted for entries (INV-06).
- A transport error leaves the intent `Unknown`; fills cannot be applied to it
  until reconciliation resolves it.
- Protection is placed as ordinary reducing orders: a stop-market stop and a
  limit target sharing an OCO group. This adapts the spec's
  `place_protection` port shape; brokers map an OCO group to their native
  OCO/GTT facility where they have one (Phase 6/9).

### Live gate (INV-14)

- Risk-increasing live orders need all five conditions. Risk-reducing live
  orders need only the `live-orders` build and `environment = production`,
  so disarming live trading can never trap an open position (INV-02).
- **Defaults: the `live-orders` feature is off, the environment is
  `development`, live trading is disabled, accounts are not armed.**

### Decision Engine

- Gate order: stale data (the last bar must be the expected last completed
  date), regime, no setup (not journaled as a decision), already in position,
  evidence, proposal, Risk Gate. Every decision, NO TRADE included, is
  journaled before an authorization is released (INV-05).
- Evidence: `EvidencePolicy::Required { min_evidence }` for paper and live.
  `EvidencePolicy::ResearchPrior` (all probability on the time exit, EV 0)
  exists for research backtests that produce the evidence; the engine refuses
  it for any non-backtest account. Research backtests use
  `research_risk_config`, a copy of the risk configuration with the minimum
  EV set to 0; every other limit is unchanged.
- Costs in a proposal are quoted at a reference quantity: the risk budget
  over the gross risk per unit, never below the minimum order quantity. The
  Risk Gate re-prices at the final quantity (ADR 0005).
- `E[P&L | time exit]` comes from evidence in R and is converted with the
  gross risk per unit.

### Position Manager

- After an entry fills completely it places the OCO stop and target. If the
  stop cannot be placed the position is `Unprotected` and reported for an
  alert (it then counts at the gap shock).
- Time exits and invalidation are checked on completed bars; the exit is a
  market order at the next open, sent after cancelling the protection.
- Reconciliation reports mismatches and never trades. Position events are
  journaled best effort; orders are journaled strictly by the gateway.

### Backtesting

- The simulated broker implements the same executor port the live broker
  will; the engine runs the same Decision Engine, Risk Gate, Order Gateway
  and Position Manager (INV-08).
- Fill rules (conservative) are documented in `qd-backtest/src/sim.rs`:
  eligibility from the next bar, gaps fill at the open, stop-limit entries do
  not chase gaps, day orders expire, a bar touching both stop and target
  fills the stop, stops placed on the entry bar are checked against it.
- A version is simulated at the stage given in the backtest configuration.
  Validation runs of a Research-stage version simulate it at Paper; the Risk
  Gate refuses non-trading stages even in backtests.
- The in-memory journal and halt store are real implementations used by
  backtests. The server will use PostgreSQL (Phase 4).
- **Limitations:** one currency per backtest (no FX series); costs are
  charged when a trade closes; the shipped cost schedules start on
  2026-01-01, so backtests over earlier dates fail closed (NO TRADE,
  configuration) until historical schedule versions are added.

## Consequences

- Phase 4 adds persistence (journal, halts, positions, intents) behind these
  ports and the startup sequence (halted until reconciliation).
- Phase 7 builds walk-forward, holdout and Monte Carlo on `run_backtest`.
