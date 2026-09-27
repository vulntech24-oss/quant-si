QuantDesk — Build Specification and Instructions for Claude Code
You are the lead engineer for QuantDesk in this repository. You own the architecture, implementation, tests and documentation. Work incrementally, verify everything you build, and report honestly. This document is your specification. Read all of it before you change anything.

This spec belongs in the repository at `docs/QUANTDESK_BUILD_SPEC.md`. If it is not there yet, save it there verbatim before doing anything else. Re-read the relevant sections at the start of every phase and whenever your context has been summarized.

**What success looks like:**

- A Rust modular monolith (Tokio, Axum, SQLx, PostgreSQL, Redis) connected to the existing Stitch-generated frontend.
- Validated, versioned strategies produce complete trade proposals.
- A deterministic Decision Engine and Risk Gate decide what happens.
- A single Order Gateway, with the kill switch enforced inside it, executes through a broker abstraction (simulated, paper, Zerodha Kite).
- A Position Manager protects and exits positions and reconciles with the broker.
- Every decision, including NO_TRADE, is journaled and traceable.
- The identical decision logic runs in backtest, paper and live.
- AI providers contribute evidence and explanations, but never control money.

Optimize for correctness, safety, modularity, maintainability, extensibility, testability and a simple user experience. Do not optimize for volume of code.

---

## 1. How you work

**1.1 Inspect before changing anything.**

- Audit the repository: directory structure, languages, build tools, any existing backend code, environment files and conventions, Docker files, CI, and Git status and history.
- Audit the frontend: whether it's a framework or static HTML, its build system, routes and pages, components, styling and Tailwind configuration, assets, icons and fonts, mock data, API assumptions, and tests.
- Identify what already works and what conflicts with this spec.

**1.2 Plan, then stop at Checkpoint A.** Phase 0 (Section 17) produces:

- the audit
- the target architecture adapted to this repository
- ADRs for key decisions
- an API contract draft mapped to the existing screens
- the phase plan
- the owner decisions in Section 4, with your recommendations

Then stop and wait for my approval. Write no production code before I approve.

**1.3 Work phase by phase (Section 17).**

- For each phase: plan it briefly, implement it, verify it, document it, and report it (Section 20).
- Start the next phase only when the current phase's exit criteria pass.
- When an earlier phase needs an interface that a later phase completes, build a real, working implementation for the current scope (for example, the simulated broker). Never build a placeholder.

**1.4 Verify continuously.** Run all of these at the end of every phase and before every commit, and fix whatever fails:

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo test --workspace`
- `cargo sqlx prepare --workspace --check` (once SQLx query macros exist)
- `cargo deny check` (advisories, licenses, bans)
- the frontend's own lint, type-check, build and test commands

Never disable a test or lint to get a green build. If a lint is a genuine false positive, allow it locally with a comment explaining why.

**1.5 Keep durable memory in the repo.**

- Create and maintain a concise `CLAUDE.md`, at most about 150 lines. It covers: 
  - what QuantDesk is
  - build, test and run commands
  - a map of where code lives
  - the Section 3 invariants, one line each
  - links to this spec, the ADRs and the progress log
  - the current phase
- Maintain `docs/PROGRESS.md`: phase status, what was verified and how, open issues, and next steps.
- Update both files at the end of every phase.

**1.6 Record decisions.** Write an ADR at `docs/adr/NNNN-title.md` (context, decision, alternatives, consequences) for every significant choice and every deviation from this spec.

**1.7 Be honest about what works.**

- Never claim something works unless you ran it and saw it work.
- Never present mock data as real, and never fake an integration.
- Use fakes and mocks only where external services or credentials are unavailable. Name them clearly (`Fake*`, `Mock*`) and keep them in test-support code or behind a `dev-fakes` feature. The server must refuse to start in production if any fake is configured.
- No `todo!()`, `unimplemented!()`, `dbg!()` or placeholder logic in runtime code paths. Deny `clippy::todo`, `clippy::unimplemented` and `clippy::dbg_macro`.

**1.8 Stay safe during development.**

- Paper mode is the default everywhere.
- Never place a real order, and never call live order endpoints.
- Never ask me to paste secret values into this conversation. Tell me which variables to set in my local `.env` instead.

**1.9 Respect existing work.**

- Don't overwrite or delete files without a reason you can state.
- Preserve the frontend's design (Section 10).
- Prefer extending over rewriting.
- Raise conflicts at Checkpoint A with a proposed resolution.

**1.10 Use Git well (if available).**

- Make small, logically separate commits with conventional-commit messages, only when all checks pass.
- Never commit secrets, `.env` files or local data.
- Never rewrite history.

**1.11 Read current documentation before each external integration.**

- Before implementing Kite Connect, OpenAI, Google Gemini, xAI or the crypto data source, read the provider's current official documentation. Fetch it if you have web access; otherwise ask me.
- Record the API version, endpoints used, limits, constraints and assumptions in `docs/integrations/<provider>.md`.
- Provider facts in this spec were current when it was written. Verify each one before relying on it.

**1.12 Decide versus ask.** Make reasonable engineering decisions yourself and record them in ADRs. Ask me only about the owner decisions in Section 4 and about genuine blockers.

---

## 2. Product and scope

QuantDesk is a quantitative investment and trading intelligence platform built around one loop:

**Analyze → Find opportunity → Predict → Decide → Validate → Manage risk → Execute → Monitor → Learn**

It is not an HFT terminal, not a candlestick-prediction app, and not a Bloomberg-style data dump. It is sophisticated inside and simple outside.

**V1 scope:**

- **Trade type:** defined-risk trades (entry, stop loss, target, maximum holding period) lasting days to weeks, evaluated on completed daily bars. The architecture stays open to more asset classes, strategies, brokers and timeframes.
- **Assets:** stocks, gold, silver, crude oil and crypto. Section 4 covers what the broker can actually trade.
- **Users:** one owner, plus optional read-only users. QuantDesk is not a multi-tenant product and not a system for distributing recommendations to third parties.
- **NO_TRADE:** a first-class, frequent and respectable outcome. The system must never feel pressure to trade.
- **User experience:** decision first → why → trade details → risk → deep analysis → advanced/raw data.

When goals conflict, prioritize: **correctness → safety and risk control → explainability → validation → maintainability and simplicity → features.**

---

## 3. Non-negotiable invariants

Enforce each invariant in code: through types, module and crate boundaries, and database constraints wherever possible. Cover each one with tests named `invariant_NN_*`.

- **INV-01 One order path.** 
  - Every broker order goes through the Order Gateway: automated entries, exits, protective orders, manual orders, flatten requests, and orders from any future strategy or broker.
  - No other code can reach order-placing broker calls. Enforce this structurally, with a trait split and crate visibility (Section 5.4).
- **INV-02 The kill switch blocks risk, never exits.** 
  - While any applicable halt is active, the Gateway rejects risk-increasing orders.
  - Risk-reducing orders (exits, protective stops and targets, flatten) are never blocked by a halt, but they are still validated.
  - An exit can never exceed the open quantity, so it can never reverse a position.
- **INV-03 Risk overrides everything.** The Risk Gate can reject any entry, and nothing upstream (strategy, AI, UI) can bypass it. Manual entries pass through the same Risk Gate.
- **INV-04 AI is advisory.** 
  - AI never places or cancels orders, sizes positions, reads broker credentials, changes risk limits or strategy parameters, controls the kill switch or the Gateway, or retrains or redeploys anything.
  - AI influences a decision only as far as that strategy version's AI policy allows. The default policy is no influence, with shadow logging only (Section 7.5).
- **INV-05 No journal, no trade.** Decisions and order intents are durably written before any broker call. If the write fails, nothing is sent and entries halt.
- **INV-06 Fail closed.** 
  - Missing, stale or inconsistent inputs mean entries are rejected. This covers prices, bars, equity, positions, FX rates, calendars, configuration and kill-switch state.
  - An unknown kill-switch state counts as halted.
- **INV-07 Restart conservatively.** On startup, entries stay halted until reconciliation and health checks pass. Hard halts survive restarts until a human re-arms them.
- **INV-08 One logic, three modes.** 
  - Strategy logic, feature computation, proposal building, decision rules, risk rules, gateway rules and the cost model are the same code in backtest, paper and live.
  - Only the clock, the data source and the broker adapter differ.
- **INV-09 Point-in-time only.** 
  - A decision uses only data knowable at its decision timestamp, and strategies see completed bars only.
  - Every decision references an immutable data snapshot and can be replayed to the identical result.
- **INV-10 Versioned strategies.** 
  - A registered strategy version's parameters and logic are immutable. Any change creates a new version that restarts validation.
  - Only validated versions at an eligible stage can trade, with capital capped by stage.
- **INV-11 Human-gated promotion, automatic demotion.** Moving a strategy to more capital requires an authenticated owner approval backed by recorded evidence. Demotion on breach is automatic.
- **INV-12 Explicit trade semantics.** The actions are OpenLong, CloseLong, OpenShort and CloseShort. There is no ambiguous "SELL" anywhere in the domain, and the UI always says which action it means.
- **INV-13 Exact arithmetic.** 
  - Prices, quantities and money use decimal types, never binary floating point.
  - Levels and quantities are rounded to the instrument's tick and lot rules before RR, EV and size are finalized.
- **INV-14 Live trading is opt-in several times over.** A live order requires all of the following. The default everywhere is paper. 
  - a build with the `live-orders` Cargo feature
  - `environment = production`
  - live trading enabled in configuration
  - an account `live_armed` flag set through a step-up-authenticated owner action
  - a strategy version at the SmallCapital or Full stage
- **INV-15 Secrets stay on the server.** 
  - Secrets are never sent to the frontend, never logged and never committed.
  - The frontend talks only to the QuantDesk backend, never to Kite or to AI providers.
- **INV-16 History is append-only.** Journal entries, order events, kill-switch events, strategy stage transitions and audit logs can't be updated or deleted, and the database enforces this.
- **INV-17 The headline is post-risk.** Every user-facing decision display shows the verdict after risk checks. A signal that Risk blocked is shown as NO TRADE together with the reason for the block.

---

## 4. Owner decisions (present at Checkpoint A)

Give me your recommendation for each item. If I don't answer an item, proceed with the recommended default and record it in an ADR.

1. **Crypto venue.** 
   - Zerodha does not offer cryptocurrency trading, so Kite can't execute crypto.
   - Recommended for V1: support crypto for data, analysis, backtesting and paper trading through a separate market-data adapter for a public venue I choose. Defer live crypto execution to a future broker adapter.
   - Ask me which venue.
2. **Gold, silver and crude instruments.** 
   - On Zerodha these trade as MCX futures, which have lot sizes, multipliers, expiries and margin. Gold and silver can also be traded through NSE-listed ETFs in the cash segment.
   - Recommended: the instrument model supports both.
   - Default universe: gold and silver through ETFs. Crude oil through MCX futures (front month, configurable roll rule), paper-only until broker-side protection on MCX is verified.
3. **Short selling.** 
   - Retail accounts can't hold Indian cash equities short overnight.
   - Recommended: V1 is long-only for cash equities and ETFs.
   - Shorts only on instruments whose capability flags permit overnight shorts (for example futures), and only for strategy versions validated on shorts.
4. **Stock universe.** 
   - Which list to use (for example, the constituents of a liquid index), maintained as a dated file.
   - Broker data usually omits delisted stocks, so reports must disclose the resulting survivorship risk.
5. **Kite Connect plan.** Market data, live and historical, requires the paid Kite Connect subscription. Zerodha states that its free "Personal" apps include no market data. Confirm which plan I have.
6. **Hosting.** Placing orders through the API requires a static IP whitelisted in the Kite developer console (Section 7.11). Confirm the VPS and its static IP.
7. **Delivery exits.** 
   - Automated sell orders on delivery holdings may require a standing authorization (such as DDPI) on the Zerodha account. Verify this.
   - Until it's confirmed, treat live equity entries as blocked, because their exits wouldn't be guaranteed.
8. **AI providers.** Which of OpenAI, Gemini and xAI to enable first, their model IDs (configuration only), and daily and monthly budget caps.
9. **Initial risk parameters.** Confirm or change the configuration defaults below.
10. **Frontend approach.** If the Stitch output is static HTML, confirm the integration approach you propose (Section 10).
11. **FX source.** Needed for non-INR instruments, for example crypto quoted in USD or USDT.

**Default risk configuration.** These are placeholders that live in config, not code.

- **Risk per trade:** 0.5% of equity.
- **Maximum total open risk:** 5%.
- **Open-risk caps:** 2% per correlated bucket; 2% per strategy version.
- **Loss limits:** 
  - daily: 2% of start-of-day equity, open P&L included
  - weekly: 4%
  - hard halt at 10% drawdown from the equity high-water mark
- **Consecutive-loss cool-off:** after 3 losing trades. Configurable; the original brief used 2.
- **Stage risk multipliers for live accounts:** 
  - Paper: 0, meaning not eligible for live
  - SmallCapital: 0.25
  - Full: 1.0
  - Paper accounts size positions as if at Full, so paper results are comparable with the backtest.
- **Minimum evidence:** 30 comparable out-of-sample setups.
- **Minimum expected value:** +0.10R after costs.
- **RR floor:** set per strategy.

---

## 5. Architecture

### 5.1 Flow

```
FOUNDATION (point-in-time): instruments · calendars · daily bars · events · FX · cost & slippage models
                            · data snapshots · feature engine · deterministic regime classifier
      │ history                                                        │ live
      ▼                                                                │
VALIDATION (offline, no broker access): full-system backtest → walk-forward → locked holdout
                                        → Monte Carlo → paper → small capital → full
      ▼                                                                │
STRATEGY REGISTRY: immutable versions · stages · capital caps · evidence tables · halt thresholds
      │ only eligible versions                                         ▼
OPPORTUNITY SCANNER → TRADE PROPOSAL ENGINE → DECISION ENGINE (+ AI ORCHESTRATOR, per policy)
  → RISK GATE → ORDER GATEWAY (halts · hard limits · idempotency) → BROKER ADAPTER (sim | paper | kite)
  → POSITION MANAGER (protection · exits · reconciliation) → DECISION JOURNAL → REVIEW
  → candidate change → NEW STRATEGY VERSION → VALIDATION

DEEP ANALYSIS: on demand for promising opportunities; display only; never changes a decision.
MONITORING: scheduling, health, data staleness, broker session, reconciliation, halts, alerts.

```

### 5.2 Components

| Component             | Owns                                                                                            | Must never                                             |
| --------------------- | ----------------------------------------------------------------------------------------------- | ------------------------------------------------------ |
| Foundation / Data     | instruments, calendars, bars, events, FX, cost and slippage models, snapshots, features, regime | make trading decisions                                 |
| Strategy Registry     | strategy families and immutable versions, lifecycle, evidence tables, halt thresholds           | allow in-place edits of a version                      |
| Opportunity Scanner   | scheduled scans, universe filtering, ranking                                                    | call AI, or read incomplete bars                       |
| Trade Proposal Engine | complete, validated proposals with per-unit economics                                           | size positions                                         |
| AI Orchestrator       | provider calls, schemas, validation, budgets, caching, logging                                  | touch orders, sizes, limits, parameters or credentials |
| Decision Engine       | deterministic gates and decision outcomes                                                       | bypass the Risk Gate                                   |
| Risk Gate             | sizing, limits, allocation, approval or rejection                                               | depend on AI output                                    |
| Kill Switch / Halts   | halt state, triggers, re-arming                                                                 | block risk-reducing orders                             |
| Order Gateway         | the only order path: validation, idempotency, broker calls                                      | contain strategy or sizing logic                       |
| Broker adapters       | broker and venue specifics                                                                      | leak broker types into the core                        |
| Position Manager      | fills, positions, protection, exits, reconciliation                                             | trade automatically to "fix" a mismatch                |
| Decision Journal      | append-only decisions, outcomes, trace                                                          | be mutable                                             |
| Review                | calibration, parity, drift, slippage and filter reports                                         | change live behavior                                   |
| Validation / Backtest | simulation, walk-forward, holdout, Monte Carlo, evidence                                        | keep its own copy of decision or risk logic            |
| Deep Analysis         | on-demand research and explanation                                                              | change a decision                                      |
| Monitoring            | scheduling, health, staleness, alerts                                                           | hide failures                                          |

### 5.3 Modular monolith and workspace

Build one server binary and one admin CLI in a Cargo workspace, with no microservices. Crate boundaries enforce the dependency rules at compile time.

The layout below is a starting proposal:

- Adapt it to the repository after the audit, and keep the number of crates proportional to the code. Merging the adapter crates into a single infrastructure crate is acceptable.
- Record the final layout in an ADR.

```
backend/
  Cargo.toml                # workspace: shared [workspace.lints] and dependency versions
  migrations/
  crates/
    qd-domain/        # pure types, formulas, rounding, state machines, domain errors (no I/O, no tokio)
    qd-strategy/      # feature engine, deterministic regime classifier, Strategy trait + strategies (pure)
    qd-risk/          # sizing, limits, allocation, halt evaluation (pure)
    qd-app/           # use cases + orchestration; defines ALL ports (traits): scanner, proposals,
                      # decisions, gateway, position manager, journal, review, validation orchestration
    qd-backtest/      # simulated clock and broker, fill rules, metrics, walk-forward, Monte Carlo
    qd-ai/            # AI orchestrator implementation + OpenAiProvider, GeminiProvider, GrokProvider
    qd-broker-kite/   # KiteBroker: REST + WebSocket adapter (market data, account reads, order executor)
    qd-broker-paper/  # paper broker on live quotes using the shared fill model
    qd-marketdata/    # other market data sources (crypto venue, CSV import)
    qd-store/         # PostgreSQL repositories (SQLx) + Redis adapter
    qd-api/           # Axum routes, DTOs, auth, OpenAPI, SSE/WebSocket
    qd-server/        # binary: configuration, wiring, scheduler, startup checks
    qd-cli/           # admin binary (clap): migrate, import, backtest, replay, halt/re-arm, reconcile

```

Dependency rules:

- `qd-domain` depends on no internal crate. `qd-strategy` and `qd-risk` depend only on `qd-domain`.
- `qd-app` depends on domain, strategy and risk, and defines every port it needs.
- Adapter crates (`qd-store`, `qd-ai`, `qd-broker-*`, `qd-marketdata`) implement ports defined in `qd-app`.
- `qd-backtest` reuses `qd-app`, `qd-strategy` and `qd-risk` through their public interfaces. It never duplicates their logic.
- `qd-api` depends on `qd-app` only. It never touches SQL, broker adapters or AI adapters directly.
- Only the binaries (`qd-server`, `qd-cli`) wire adapters to ports.
- Route handlers do exactly three things:
  1. parse and validate the request
  2. authorize the caller
  3. call a use case and map the result to a DTO
  Business logic never goes in handlers or in SQL.

### 5.4 Ports (reference shapes: adapt the names, keep the semantics)

```rust
pub trait Clock: Send + Sync { fn now(&self) -> DateTime<Utc>; }   // SystemClock, SimClock

#[async_trait]
pub trait HistoricalMarketData: Send + Sync {
    async fn instruments(&self) -> Result<Vec<InstrumentSpec>, MarketDataError>;
    async fn daily_bars(&self, id: InstrumentId, range: DateRange) -> Result<Vec<Bar>, MarketDataError>;
}

#[async_trait]
pub trait LiveMarketData: Send + Sync {
    async fn subscribe(&self, ids: &[InstrumentId]) -> Result<QuoteStream, MarketDataError>;
}

/// Read-only broker access: positions, holdings, orders, protective orders, margins, session status.
#[async_trait]
pub trait BrokerAccountReader: Send + Sync { /* … */ }

/// Order-placing capability. Constructed only in the binaries and handed ONLY to the Order Gateway.
#[async_trait]
pub trait BrokerOrderExecutor: Send + Sync {
    async fn submit(&self, req: &BrokerOrderRequest) -> Result<BrokerOrderAck, BrokerError>;
    async fn cancel(&self, id: &BrokerOrderId) -> Result<(), BrokerError>;
    async fn place_protection(&self, req: &ProtectionRequest) -> Result<ProtectionAck, BrokerError>;
    async fn modify_protection(&self, req: &ProtectionModify) -> Result<ProtectionAck, BrokerError>;
    async fn cancel_protection(&self, id: &ProtectionId) -> Result<(), BrokerError>;
}

#[async_trait]
pub trait AiProvider: Send + Sync {
    fn id(&self) -> ProviderId;
    async fn complete_structured(&self, req: StructuredRequest) -> Result<StructuredResponse, AiError>;
}

```

- Use `async_trait` (or an equivalent) wherever trait objects are needed.
- Repositories are per-aggregate traits defined in `qd-app` and implemented in `qd-store`.
- Strategy code, risk code and the decision core never read the system clock. Time is always passed in.

---

## 6. Domain model and core semantics

### 6.1 Identifiers, time, numbers

- **IDs:** typed newtypes backed by UUIDv7, generated in the application.
- **Timestamps:** stored as UTC `timestamptz`. Trading dates are dates on a venue calendar. Display in IST (Asia/Kolkata) unless configured otherwise.
- **Numbers:** use `rust_decimal::Decimal` newtypes for money (`Money { amount, currency }`), prices, quantities, basis points and percentages.
- **Floats:** `f64` is allowed only inside statistics (for example Sharpe or Monte Carlo). Float values never flow back into prices, quantities or money.

### 6.2 Instruments

`InstrumentSpec` is data, never hardcoded. Specs are versioned with effective dates. Each spec includes:

- **Identity:** id, symbol, venue (NSE, BSE, NFO, MCX, a crypto venue, …), asset class (Equity, PreciousMetal, Energy, Crypto), kind (CashEquity, Etf, Future, Spot), underlying.
- **Contract terms:** currency, tick size, lot size, multiplier, quantity step and minimum, expiry.
- **Grouping and references:** trading-calendar id, correlation bucket, broker references (symbol and token per broker).
- **Capabilities:** 
  - `can_short_overnight`
  - `supports_market_orders`
  - `requires_market_protection`
  - `protection_modes` (for example broker OCO, broker stop only, none)
  - allowed products, order types and validities

### 6.3 Actions, outcomes and reasons

```rust
pub enum TradeAction { OpenLong, CloseLong, OpenShort, CloseShort }

pub enum DecisionOutcome {
    Enter { action: TradeAction },                       // OpenLong | OpenShort only
    Exit  { action: TradeAction, reason: ExitReason },   // CloseLong | CloseShort only
    Hold,
    NoTrade { reason: NoTradeReason },
}

```

**UI labels.** Only broker adapters map actions to broker BUY/SELL transaction types.

| Outcome    | Label                        |
| ---------- | ---------------------------- |
| OpenLong   | "BUY (open long)"            |
| CloseLong  | "SELL (close long)"          |
| OpenShort  | "SELL SHORT (open short)"    |
| CloseShort | "BUY TO COVER (close short)" |
| Hold       | "HOLD"                       |
| NoTrade    | "NO TRADE"                   |

**`NoTradeReason`** is a typed enum that carries data. At minimum it covers:

- StrategyInactiveInRegime, InvalidTradePlan
- InsufficientEvidence{n, min}, InsufficientEdge{ev_r, min}, RiskRewardBelowFloor{rr, floor}
- ConflictingSignals, Superseded, AlreadyInPosition, ShortNotPermitted
- StaleData, MarketClosedOrHalted, PriceMovedPastEntry{watch}
- RiskLimit(..), KillSwitch(..), PositionTooSmall, UneconomicAfterCosts, EventRisk, AbnormalMarket
- AiVeto, only where the strategy's AI policy allows it

"No setup" is not a decision. The scanner records it compactly (Section 7.3).

**`ExitReason`:** StopHit, TargetHit, TimeExit, Invalidated, Manual, Flatten, Roll, Demotion.

### 6.4 Trade proposal

A proposal is complete or it is not a proposal. It carries at least:

- **Identity and time:** id, created_at, decision timestamp (as-of), account mode.
- **What:** instrument, action (OpenLong or OpenShort), setup type, grade.
- **Who:** strategy id and name, strategy version id and number, logic version, git SHA.
- **Plan:** entry (price and order type), stop loss, target, time exit (maximum holding period in trading days), invalidation rules. All levels are rounded to the tick size.
- **Economics per unit, gross and net:** 
  - reward and risk
  - costs by line item, with the cost-model version
  - slippage assumptions, with the model version
  - net RR
  - outcome probabilities (target first, stop first, time exit), with their source and evidence count
  - expected value per unit and in R
- **Explanation:** structured reasons (factor, value, direction), the strongest argument against the trade, and an optional AI review reference.
- **Reproducibility:** data snapshot id, feature-set version, calendar version.

A generic "BTC will go up with 78% confidence" is never a proposal. Every probability refers to this exact plan. Proposals carry per-unit economics only; the Risk Gate decides the quantity.

### 6.5 Formulas

These live in `qd-domain`, documented and unit- and property-tested, all in Decimal. The long case is shown; shorts mirror it.

**Per-unit economics**

- `risk_gross = entry − stop`, which must be > 0.
- `reward_gross = target − entry`, which must be > 0.
- `costs`: round-trip costs from the shared cost model for the instrument, product, quantity and date, with every line item included.
- `stop_slippage`: modeled adverse slippage on stop exits (≥ 0), including a gap allowance.
- `risk_net = risk_gross + costs_per_unit + stop_slippage`.
- `reward_net = reward_gross − costs_per_unit`. A result ≤ 0 makes the plan invalid.
- `RR_net = reward_net / risk_net`.

**Expected value**

- `EV = p_target × reward_net − p_stop × risk_net + p_time × E[net P&L per unit | time exit]`, where `p_target + p_stop + p_time = 1`.
- `EV_R = EV / risk_net`.

**Sizing**

1. `qty_raw = equity × risk_per_trade × stage_multiplier / risk_net`, in account currency, using point-in-time FX.
2. Round `qty_raw` down to the lot or quantity step.
3. Cap the result by every limit in 7.8.
4. Recompute costs at the final quantity, because fixed per-order fees can make small positions uneconomic. Then re-check RR and EV.
5. Below the minimum tradable size → `NoTrade(PositionTooSmall)`.

**Open risk** (account currency)

- `Σ max(0, qty × (mark − stop))` over long positions (mirrored for shorts), plus the planned risk of working entry orders.
- A position without valid protection counts at `notional × gap_shock(asset class)` and raises an alert.

**P&L, drawdown and R-multiple**

- Daily P&L: `equity_now − equity_at_day_start`, where equity = cash + mark-to-market. The day boundary is a configured cut-off time, because crypto never closes.
- Drawdown: `(high_water_mark − equity) / high_water_mark`.
- Trade R-multiple: `net P&L / (risk_net × qty)`, using the risk planned at entry.

### 6.6 State machines

Implement each as an explicit enum with a transition function that returns `Result`. Illegal transitions are errors, and every transition is persisted as an event.

**Strategy version**

- Draft → Research → (Rejected | ResearchPassed) → Paper → SmallCapital → Full.
- Any stage → Retired.
- Paper, SmallCapital or Full → Suspended (manual) → back to the previous stage.
- Automatic demotion moves SmallCapital or Full to Paper, with a reason.

**Order intent**

- Created → GatewayRejected, or PendingSubmit → Submitted → PartiallyFilled → Filled | Cancelled | BrokerRejected | Expired.
- PendingSubmit or Submitted → Unknown (timeout or transport error), resolved only through reconciliation.
- Terminal states are immutable.

**Position**

- Opening → Open → Protected ⇄ Unprotected → Exiting → Closed.

**Halt**

- A halt is `{ kind: Manual | CoolOff | HardHalt | Operational | Startup, scope: Global | Account | StrategyVersion | Instrument, reason, started_at, auto_clear_at, requires_manual_rearm, cleared_at, cleared_by }`.
- An entry is allowed only if no active halt covers its account, strategy version or instrument.
