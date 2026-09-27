# ADR 0016: The AI agent as the desk's central intelligence

- Status: accepted
- Date: 2026-09-27

## Context

The owner decided that AI is the central intelligence of QuantDesk, not an
advisory side module. In the owner's words:

- "The AI should independently research the market using its own
  internet/web access and decide what opportunities exist."
- Capital allocation is AI-driven. Deterministic risk controls stay the
  final boundary: "AI says ₹40,000 → Risk Engine says maximum ₹25,000 →
  execute maximum ₹25,000. The AI must never bypass these hard limits."
- "Keep broker execution and hard risk limits deterministic and
  auditable."
- Every prediction is recorded and scored: prediction → horizon → actual
  result → accuracy.
- No mandatory quant layer, no rule-based prediction and no required
  backtest in the live pipeline. Existing modules are reused as tools, not
  rebuilt.

This conflicts with two invariants as written:

- **INV-04:** AI is advisory; it never touches orders or sizes.
- **INV-10:** only validated strategy versions at trading stages trade.

This ADR records how they change for the agent, and what stays fixed.

## Decisions

### What changes (owner-directed)

**INV-04, for the agent only**
- The agent may request entries and exits and propose a capital allocation.
- It still never touches limits, parameters, credentials, halts, strategy
  versions or settings. No tool exists for any of these.
- `qd-ai`, the shadow advisors, is unchanged and still advisory; its
  dependency test still holds.

**INV-10, for the agent only**
- The agent trades without a validation backtest. Its identity is a
  strategy version per model and prompt version. It is stable, derived
  from `agent-v1:<provider>:<model>`, and never registered as a
  rule-based strategy.
- Its evidence is its own scored record:
  - **Paper book:** the agent trades at stage Paper.
  - **Live book:** stage SmallCapital, and only when the owner switched it
    on in the server file (`[agent_live] enabled`) and the scored record
    meets every threshold. Defaults:
    - at least 30 scored predictions;
    - accuracy of at least 0.55;
    - a Brier score of at most 0.25;
    - at least 20 closed trades with a positive net P&L.
  - **Otherwise:** the stage is Paper, which the Risk Gate refuses on a
    live account.
- Rule-based versions keep INV-10 and ADR 0010 unchanged.

### What stays fixed (every other invariant)

**One entry path.** A trade request becomes a `TradeProposal` from the
agent's plan and probabilities. It then goes through the same components as
every strategy (INV-01, INV-03, INV-05, INV-08):

1. the `DecisionEngine` (`decide_agent`);
2. the Risk Gate;
3. the journal;
4. the Position Manager;
5. the Order Gateway.

The paper and live runners expose it (`AgentDesk`), and each call brings the
book up to date under the run lock first.

**The allocation can only shrink.** Two steps:
1. The agent's INR allocation is capped at `max_position_fraction` of
   equity (default 25%).
2. The allocation becomes a quantity ceiling (`EntryRequest.max_quantity`).
   The gate sizes by risk as always, then caps at that ceiling. It never
   raises. The approval records `capped_by_request`.

**The Risk Gate still decides everything else, unchanged**
- halts and an unknown halt state;
- stage;
- net reward-to-risk of at least `agent.min_rr` (default 1.5);
- expected value of at least `min_ev_r`, from the agent's probabilities
  (source `ai:<provider>:<model>`);
- daily, weekly and drawdown limits and the cool-off;
- risk per trade;
- total, bucket and per-version open-risk caps (all agent trades share one
  version, so `max_strategy_open_risk` bounds the agent's total open risk);
- verified costs on live accounts.

A refusal is a normal NO TRADE decision with its reason (INV-17).

**Other fixed points**
- **Exits:** the agent can close only the positions it opened, or cancel
  their unfilled entries. Exits are never blocked by halts (INV-02).
- **Live orders** still need every INV-14 condition in the Order Gateway.
- **Point in time:** completed daily bars only. Quotes inform the agent, but
  plans are judged on the last completed bar and paper entries fill on the
  next bar (INV-09).

### The agent's tools (`qd-agent`)

**Research and data**
- `web_research`: one call to the provider with only its built-in search
  tool (`web_search`, or `googleSearch` on Gemini). The result is marked
  untrusted, with its sources.
- `search_instruments` and `add_instrument`: Zerodha's list, NSE and BSE
  cash equity. The spec is delivery only with no overnight short, and 500
  days of history are imported through the usual data-quality checks. The
  addition is audited as `ai-agent`.
- `list_instruments`.
- `get_quotes`: `/quote/ohlc`.
- `get_candles`: stored bars, refreshed from Zerodha first.
- `technical_snapshot`: features-v1, the regime, returns and ranges, and what
  each catalog strategy sees. It is one input, not a gate.

**Book and record**
- `get_portfolio`: the book, limits and portfolio view.
- `get_track_record`.
- `record_prediction`.

**Trading**
- `place_trade`.
- `close_position`.

A tool never fails a run: errors go back to the model as data.

### Runs

- **Kinds:**
  - `research`: find opportunities, predict, trade;
  - `monitor`: review positions while NSE is open;
  - `manual`: the owner's request.
- **Budgets:**
  - model turns (24);
  - tool calls (40);
  - trade requests (3);
  - wall-clock time (600 s).
- One run at a time (run lock `agent-run`).
- Every run is journaled as `agent_run` with its full, bounded tool trace,
  the decisions it produced and the predictions it made.
- **Schedules:** a daily research time and a monitoring interval, both off
  by default. A Telegram message is sent when a run made trade requests.

### Predictions and learning

- **Recording:** each trade request records an `ai_prediction` with the
  plan's target and stop, its horizon and `p_target`. Opportunities that
  were not traded are recorded with `record_prediction`.
- **Scoring:** deterministic, from completed daily bars
  (`ai_prediction_outcome`):
  - when a target and a stop are stated, the first one touched decides; a
    bar touching both counts as the stop;
  - otherwise the direction of the close at the horizon decides.
- **Scorecard:**
  - accuracy;
  - Brier score;
  - calibration bands;
  - results of the linked trades.
  It is kept per model and fed back to the agent through
  `get_track_record`. The live gate reads it.

### Prompt injection and safety

- Web content is marked untrusted, and the instructions forbid following
  it.
- Even a fully misled model can do no more than request entries of bounded
  size through the Risk Gate, or close its own positions. There is no tool
  for limits, halts, settings, keys, strategies or accounts.
- The kill switch blocks the agent's entries like any other entry.
- The crate's dependency test forbids broker, store, server, API and risk
  crates, and names of order-path or secret types in its sources.

### Configuration

- `agent` is a settings section editable in the web UI (with step-up):
  - provider;
  - model;
  - schedules;
  - limits;
  - the owner's standing instructions.
- `[agent_live]` is set in the server file only.
- Keys are the existing `openai_api_key`, `xai_api_key` and
  `gemini_api_key`.

## Consequences

- **New code:**
  - the crate `qd-agent`;
  - journal kinds `agent_run`, `ai_prediction` and `ai_prediction_outcome`;
  - the exit reason `agent`;
  - the id types `AgentRunId` and `PredictionId`;
  - routes under `/api/agent/*`;
  - the AI agent page.
- **No migration:** the journal is kind-agnostic.
- The agent's paper results start at zero. Nothing about its accuracy is
  known until predictions are scored. NO TRADE stays a normal outcome.
- Provider cost scales with runs. The budgets and the off-by-default
  schedules keep it in the owner's hands.
