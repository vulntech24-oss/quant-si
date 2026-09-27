# AI providers (OpenAI, Gemini, xAI)

- Status: **implemented** in `backend/crates/qd-ai-providers` (ADR 0014).
  Advisory only (INV-04): the advisors return commentary in shadow mode and
  never touch orders, sizes, limits, parameters, credentials or halts.
- Read on 2026-09-27.

## OpenAI: Responses API with Structured Outputs

- Source: https://platform.openai.com/docs/guides/structured-outputs. The
  API reference page returned 403 to the fetcher; the guide has the full
  request and response shapes.
- `POST https://api.openai.com/v1/responses`, header
  `Authorization: Bearer <key>`.
- Body: `{"model": ..., "input": [{"role": "system", ...}, {"role": "user", ...}],
  "text": {"format": {"type": "json_schema", "name": ..., "schema": ..., "strict": true}}}`.
- Response: `output[]` holds an item with `type: "message"`, whose `content[]`
  holds `{"type": "output_text", "text": "<json>"}` or
  `{"type": "refusal", "refusal": ...}`.
- Default model in Settings: `gpt-6-astra`, the model used in the guide's
  examples on the read date. Change it in Settings.

## xAI: Responses API (same shape as OpenAI)

- Source: https://docs.x.ai/docs/guides/structured-outputs and
  https://docs.x.ai/docs/models.
- Base URL `https://api.x.ai/v1`, `POST /responses`, the same `text.format`
  JSON schema, and the same `output[]` message and `output_text` shape.
  Header: `Authorization: Bearer <key>`.
- Default model: `grok-4.7`, the flagship on the models page on the read date.

## Google Gemini: generateContent with a response schema

- Source: https://ai.google.dev/api/generate-content,
  https://ai.google.dev/gemini-api/docs/structured-output and
  https://ai.google.dev/gemini-api/docs/models.
- `POST https://generativelanguage.googleapis.com/v1beta/models/<model>:generateContent`,
  header `x-goog-api-key: <key>`.
- Body: `{"systemInstruction": {"parts": [{"text": ...}]}, "contents":
  [{"role": "user", "parts": [{"text": ...}]}], "generationConfig":
  {"responseMimeType": "application/json", "responseJsonSchema": ...}}`.
  The reference marks `responseSchema` as deprecated, so the adapter sends
  the JSON Schema in `responseJsonSchema`. The newer `interactions` endpoint
  in the structured-output guide is not used yet.
- Response: `candidates[0].content.parts[0].text` is the JSON.
- Default model: `gemini-3.8-flash`, listed first on the models page on the
  read date.

## How QuantDesk uses them

- One JSON schema for all three providers: `stance` (agree | caution |
  disagree | abstain), `confidence` (0–1), `summary`, `flags[]`. The draft is
  validated and bounded by `qd_ai::advisor::finalize` before it is journaled.
- The prompt is the `AdvisorInput` packet: plan, economics, probabilities,
  regime and the argument against. It holds no equity, quantities or secrets.
- Keys are entered in Settings (`openai_api_key`, `gemini_api_key`,
  `xai_api_key`) and stored encrypted. The server reads them when it builds
  an advisor; the key never reaches `qd-ai` or the browser.
- Each provider is switched on separately in Settings → Advisory AI. The daily
  call budget and the timeout apply per advisor.
