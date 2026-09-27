# ADR 0005: Phase 2 decisions: cost model, Risk Gate and first strategy

- Status: accepted
- Date: 2026-09-27

## Context

Phase 2 builds the pure Risk Gate (`qd-risk`), the cost model and the strategy
crate (`qd-strategy`). Spec §7.8 (risk limit details) is still missing, so the
Risk Gate implements the §4 defaults and §6.5 sizing only. The official
Zerodha charges page was not reachable from the build environment.

## Decisions

### Cost model

- The `CostModel` trait and the schedule-based implementation live in
  `qd-domain` (`costs.rs`). The Risk Gate must re-price costs at the final
  quantity (§6.5 step 4), and `qd-risk` may depend only on `qd-domain` (§5.3).
- Rates are data: `backend/config/costs/india-zerodha.toml`. Each schedule
  has a version, effective dates and a verification record. **All shipped
  schedules are `unverified`**: they come from third-party summaries. The Risk
  Gate rejects unverified schedules for live accounts
  (`MissingOrInconsistentData { Configuration }`). Paper and backtest may use them.
- Charges depend on whether an order acquires or disposes of units, not on
  the side. `TradeAction::transfer()` gives `Purchase` or `Disposal`; the words
  buy/sell stay out of the domain (INV-12).
- Each amount is rounded to 2 decimals per leg, half away from zero. GST
  applies to brokerage and to charges marked `gst`.
- The exit leg is priced at the higher plan level (larger turnover), so
  estimates err on the high side.
- The depository charge applies per scrip per day; it is modeled per disposal
  order. Gold and silver ETFs are modeled without STT. Both need confirmation.
- No crypto schedule exists until a venue is chosen; quoting a crypto
  instrument fails closed (`NoSchedule`).

### Risk Gate

- Check order: halts → input consistency → stage → overnight-short
  permission → proposal gates (RR floor, minimum EV) → loss limits → sizing
  with caps and costs. The first failure is the NO TRADE reason.
- Paper and backtest accounts size as if at Full (§4). Live accounts use the
  stage multiplier; Paper stage (multiplier 0) never trades live.
- Caps (total, bucket, strategy version) shrink the quantity to the remaining
  headroom. If nothing is left, the reason is the cap breach, not "too small".
- Sizing and cost re-pricing repeat until the per-unit risk at the final
  quantity is no larger than the one used for sizing (at most 8 rounds). The
  final planned risk therefore never exceeds the budget or the caps; the size
  can be slightly below the maximum.
- An RR or EV failure found before sizing reports `RiskRewardBelowFloor` or
  `InsufficientEdge`. A failure that appears only at the final quantity's
  costs reports `UneconomicAfterCosts`.
- The RR floor is a property of the strategy version (§4 "set per
  strategy"); the request carries it.

### Halt triggers

- Drawdown at the hard-halt limit proposes an account `HardHalt` (manual
  re-arm only). Consecutive losses at the threshold propose an account
  `CoolOff`. A trigger is not repeated while an active halt of that kind
  covers the account; with the halt state unknown it is proposed anyway.
- **Assumptions (owner to confirm):** cool-off lasts 24 hours (the spec
  gives no duration); gap shocks are equity 20%, precious metals 10%, energy
  20%, crypto 30% (the spec requires them but gives no values).

### Strategy crate

- `Bar` and `BarSeries` live in `qd-domain` (`market.rs`) because ports and
  strategies share them. A series refuses bars after its last completed date
  (INV-09).
- Features (`features-v1`) and the regime classifier (`regime-v1`) are pure
  Decimal computations; their periods and thresholds belong to the version.
- A strategy returns `Inactive`, `NoSetup` or a `SetupCandidate` (raw plan,
  setup type, grade, reasons, strongest argument against). It never sizes and
  never produces probabilities: those come from evidence tables built by
  validation (Phase 7) and are joined by the Trade Proposal Engine (Phase 3).
- First strategy: trend pullback, long only, logic version
  `trend-pullback-1.0.0`, parameters fixed in `TrendPullbackParams::V1`
  including its net RR floor of 1.5. Its edge is unproven: it must pass
  validation before any stage above Research.

## Consequences

- Phase 3 wires these into use cases: the proposal engine (strategy →
  costs at a reference quantity → probabilities → `TradeProposal`), the
  decision engine (evidence and regime gates) and the Risk Gate.
- The owner must verify the cost schedules and confirm the two assumptions
  above before any live use.
