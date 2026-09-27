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

## Tool calling and web search (the AI agent, ADR 0016)

Read on 2026-09-27.

### OpenAI (Responses API)

- Sources: https://developers.openai.com/api/docs/guides/function-calling and
  https://developers.openai.com/api/docs/guides/tools-web-search (the
  platform.openai.com pages now redirect there).
- A function tool is `{"type": "function", "name": ..., "description": ...,
  "parameters": <JSON Schema object>, "strict": true}` in `tools[]`.
- The response `output[]` holds `{"type": "function_call", "call_id", "name",
  "arguments": "<JSON string>"}` items. Each result goes back as an input item
  `{"type": "function_call_output", "call_id": ..., "output": "<string>"}`.
  To continue, re-send the accumulated output items in `input` (or use
  `previous_response_id`; QuantDesk re-sends, so nothing is stored at the
  provider between turns).
- `parallel_tool_calls` and `tool_choice` ("auto", "required", ...) control
  calls.
- Web search is the built-in tool `{"type": "web_search"}` (the old
  `web_search_preview` is legacy). Options: `search_context_size`,
  `user_location`, `filters.allowed_domains`/`blocked_domains`. The output
  has `web_search_call` items and a `message` whose text carries
  `url_citation` annotations (`url`, `title`). It can be combined with
  function tools.

### xAI (Responses API)

- Sources: https://docs.x.ai/docs/guides/function-calling and
  https://docs.x.ai/docs/guides/tools/search-tools.
- Function tools, `function_call` items and `function_call_output` inputs
  have the same shapes as OpenAI's. Parameters must be an object schema.
- Web search is `{"type": "web_search"}` (options `allowed_domains` or
  `excluded_domains`, at most 5, not both). Built-in tools run on xAI's
  servers; custom functions pause and come back to the caller.

### Gemini (generateContent)

- Sources: https://ai.google.dev/gemini-api/docs/function-calling,
  https://ai.google.dev/gemini-api/docs/tool-combination,
  https://ai.google.dev/gemini-api/docs/google-search and
  https://ai.google.dev/api/generate-content.
- Functions: `tools: [{"functionDeclarations": [{"name", "description",
  "parametersJsonSchema"}]}]`. The model answers with `functionCall` parts
  (`name`, `args`, `id`). Results go back as a `user` turn of
  `functionResponse` parts (`name`, `response` object, `id`). The model's own
  turn is sent back unchanged, so `thoughtSignature` parts survive.
- Search grounding: `tools: [{"googleSearch": {}}]`. The response carries
  `groundingMetadata.groundingChunks[].web.{uri,title}` and
  `webSearchQueries`.
- Mixing built-in tools with functions is "Preview … Gemini 3 models only",
  and is documented only on the newer Interactions API.

### How the agent uses them

- **Web research is a function tool, `web_research(query)`, for all three
  providers.** QuantDesk implements it with a separate call that has only
  the provider's search tool:
  - `web_search` on OpenAI and xAI;
  - `googleSearch` on Gemini.
  It returns the answer text plus the cited sources.
- **Why a function tool and not native mixing:**
  - every query and every source is journaled in the run trace;
  - one loop works for every provider;
  - Gemini does not need the preview combination.
- The agent loop sends QuantDesk's function tools with `tool_choice: "auto"`
  and re-sends the whole transcript on each turn.
