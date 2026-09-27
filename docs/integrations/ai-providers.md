# AI providers (OpenAI, Gemini, xAI)

- Status: **not implemented**. Checked 2026-09-27: `platform.openai.com`,
  `ai.google.dev`, `docs.x.ai`, `api.openai.com` and `api.x.ai` were blocked
  by the build environment's egress policy, so no adapter was written from
  memory.
- The advisory core exists (`qd-ai`, ADR 0011). An adapter implements
  `qd_ai::advisor::Advisor`:
  - it gets the `AdvisorInput` packet, which holds no equity, quantities or
    secrets;
  - it returns an `AdviceDraft`, which is validated and bounded before it is
    journaled.
- Keys stay on the server (INV-15). Set them in your local `.env`, and never
  paste them into a conversation. Proposed variable names:
  `QD_OPENAI_API_KEY`, `QD_GEMINI_API_KEY`, `QD_XAI_API_KEY`.
- Owner decisions still open (ADR 0004): which providers, models and daily
  budgets. `[ai] max_calls_per_day` caps calls per advisor.
