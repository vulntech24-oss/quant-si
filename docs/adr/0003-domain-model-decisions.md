# ADR 0003: Domain model decisions and deviations from §6

- Status: accepted
- Date: 2026-09-27

## Context

§6 defines the domain model. Implementing it exactly exposed a few gaps and
details the spec leaves open. Each is recorded here.

## Decisions

### Contract multiplier and FX in the formulas (deviation)

§6.5 writes per-unit economics, sizing, open risk and R-multiple in price
points (`risk_gross = entry − stop`). For futures that understates money at
risk by the multiplier: one MCX crude lot moves 100 × the quoted price.
Sizing 5,000 INR of risk against a 134-point stop would give 37 lots instead
of 0.

- Per-unit economics are money per unit of quantity in the instrument
  currency: `risk_gross = (entry − stop) × multiplier`, likewise reward and
  stop slippage. Costs are already money.
- Sizing divides the budget by `risk_net_per_unit` converted into the account
  currency with a point-in-time FX rate.
- Open risk is `max(0, qty × multiplier × (mark − stop))` converted into the
  account currency; unprotected positions use `qty × multiplier × mark × gap_shock`.

For cash equities (multiplier 1, INR) every formula equals the spec's.

### Quantities and instrument terms

- `Quantity` counts order units and is never negative; direction lives only
  in the action. `quantity_step` must be a whole number of `lot_size` and
  `min_quantity` a whole number of steps, so rounding to the step also
  satisfies lot rules.
- `Price` must be strictly positive. Instruments that can print negative
  prices are unsupported, so inputs fail closed (INV-06).
- A future must have an expiry, and nothing else may have one.

### Conservative tick rounding

Plan levels are rounded against the trade: for longs the entry rounds up and
the stop and target round down; for shorts the entry rounds down and the stop
and target round up. RR and EV are therefore never flattered by rounding.
Quantities always round down.

### Types enforce trade semantics (INV-12)

`DecisionOutcome::Enter` holds an `EntryAction` (OpenLong | OpenShort) and
`Exit` an `ExitAction` (CloseLong | CloseShort), so an ambiguous outcome cannot
be built. A test scans `qd-domain` sources for bare buy/sell identifiers.

### NoTradeReason

All §6.3 reasons exist. Additions and data shapes:

- `MissingOrInconsistentData { input }` next to `StaleData { input }`, because
  INV-06 covers missing and inconsistent inputs as well as stale ones.
- `PriceMovedPastEntry { entry, last }` carries both prices (the spec's
  `{watch}`).
- `RiskLimit(RiskLimitBreach)` and `KillSwitch(HaltBlock)` carry typed detail.
- Serialized as `{ "code": ..., "detail": ... }` with stable codes.

### Probabilities

`p_target + p_stop + p_time` must equal 1 exactly in Decimal. Estimators
should derive the last probability as `1 − the others`.

### State machines

- Order intent: PendingSubmit → BrokerRejected is allowed (a broker can refuse
  synchronously), and PartiallyFilled → Unknown is allowed (a transport error
  can happen at any point while an order is working). Unknown leaves only
  through a `Reconciled` event.
- Position: Opening → Closed for an entry that never fills; Exiting →
  Unprotected when an exit fails, so the position counts at the gap shock and
  alerts. Open → Unprotected when protection placement fails.
- Strategy: every promotion, including ResearchPassed → Paper, needs an
  `OwnerApproval` referencing recorded evidence and must go exactly one stage
  up. Resume returns to the suspended-from stage. Retired is terminal.
- Halts: Manual and HardHalt always require a manual re-arm and never
  auto-clear; the system can never clear them. `HaltState::Unknown` blocks
  entries. Risk-reducing orders pass any halt state.

### Serialization

Decimals serialize as JSON strings so no consumer turns them into floats.
Validated aggregates (`InstrumentSpec`) re-validate on deserialize; types
whose invariants depend on construction (`TradeProposal`, `Halt`,
`CostEstimate`) are serialize-only until storage adapters need to read them
back, which will go through validating constructors.

## Consequences

- `qd-risk` and the Order Gateway reuse these functions; nothing re-derives
  the formulas (INV-08).
- The spec's §6.5 text should be updated to include the multiplier and FX.
