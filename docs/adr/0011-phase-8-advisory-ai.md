# ADR 0011: Phase 8 decisions: advisory AI in shadow mode

- Status: accepted
- Date: 2026-09-27

## Context

INV-04: AI is advisory. It never touches orders, sizes, limits, parameters,
credentials or halts. The spec's AI policy section (§7.5) is missing, and so
is the owner's choice of providers and budgets (ADR 0004, item 8). The
provider documentation (OpenAI, Gemini, xAI) could not be fetched from the
build environment, and the project rules forbid writing an adapter without
reading it.

## Decisions

### Structure: the AI cannot act

- New crate `qd-ai`. Its dependencies are `qd-app` (ports and journal
  types) and `qd-domain`, and nothing that can place orders, size positions,
  change limits or halts, touch the registry or accounts, or hold secrets.
  A test reads the crate's manifest and sources to enforce this.
- The orchestrator's only write capability is a journal append of
  `ai_advice` entries (`JournalEntry::AiAdvice`, append-only, INV-16).
- The Decision Engine, Risk Gate, Order Gateway and Position Manager have no
  input through which advice could flow. The end-to-end test runs the AI
  after paper trading and checks that nothing else in the journal, halts,
  stage events or accounts changed.

### Shadow mode

- Advisors see **entry decisions only**, after the Risk Gate: a NO TRADE
  needs no second opinion, and this bounds cost. Each advisor advises
  once per decision; re-runs skip what is already advised.
- The input packet holds the plan, economics, probabilities, evidence count,
  regime and the proposal's strongest argument against. It **never holds
  account equity, quantities, credentials or anything secret**, so a remote
  provider learns nothing it could act on (INV-15).
- Output is validated and bounded before it is journaled:
  - confidence in [0, 1];
  - a plain-text summary of at most 1,000 characters, with control
    characters stripped;
  - at most 10 flags of 200 characters.
  The frontend renders it with `textContent` only, under the heading "AI
  review (advisory only)".
- **Scoring:** an `agree` is right when the trade made money, and a
  `caution` or `disagree` is right when it lost. The scorecard also compares
  mean R with and without the advisor's agreement. Giving any advisor
  influence on decisions would need a new ADR and an INV-04 change; nothing
  in the code allows it.

### Cost and failure control

- `[ai] enabled = false` by default.
- `max_calls_per_day` per advisor (default 200). The budget resets each UTC
  day, and over-budget decisions wait for the next day.
- `timeout_seconds` per call (default 20, at most 300).
- A failing or slow advisor fails alone: the failure is counted and logged,
  it is not retried in the same run, and it has no effect on trading.

### The first advisor: a deterministic checklist

`checklist-v1` needs no network. It flags any of these conditions, with
thresholds in `[ai.checklist]`:
- net RR below 2.0;
- EV below 0.2R;
- fewer than 60 comparable setups;
- a stop more than 10% from the entry;
- costs above 20% of the net reward;
- a high-volatility regime.

With no flags it agrees; with one or two it cautions; with three or more it
disagrees. It is a real advisor, not a stand-in: it runs the whole path
(packet, validation, journal, scoring) with no provider, and its stances are
scored like any other's.

### Running it

- It runs after the daily paper run when `daily_run_utc` is set and AI is
  enabled.
- Manually: `qd ai run|advice|scorecard` or `POST /api/ai/run` (owner).
- Reads: `GET /api/ai/advice[?decision=]` and `GET /api/ai/scorecard`.
- The frontend has an AI page and shows advice on the decision detail.

## Not done, and how to add it

To add a provider adapter:
1. Read the provider's current docs and record them in
   `docs/integrations/ai-providers.md`.
2. Implement `qd_ai::advisor::Advisor`, with the key read from the server
   environment (`QD_OPENAI_API_KEY`, `QD_GEMINI_API_KEY`, `QD_XAI_API_KEY`).
3. Register the adapter in `qd-server/src/ai.rs`.
Its advice then flows through the same validation, budget, journal and
scorecard.
