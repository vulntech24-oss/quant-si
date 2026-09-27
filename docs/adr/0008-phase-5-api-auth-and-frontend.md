# ADR 0008: Phase 5 decisions: HTTP API, authentication and frontend

- Status: accepted
- Date: 2026-09-27

## Context

Phase 5 adds the HTTP API (`qd-api`) and the frontend. The spec defines the
invariants the UI must respect (INV-15 secrets on the server, INV-17
post-risk display) and the §6.3 action labels, but §10 (frontend) and the
security sections are missing. The Stitch export in `design/stitch-reference/`
is a visual reference only.

## Decisions

### API shape

- JSON over HTTP under `/api`, served by `qd-server` on the same origin as
  the frontend, so there is no CORS configuration at all. The frontend talks
  only to this backend (INV-15); the API never calls brokers or AI providers.
- Handlers parse, authorize, call a `qd-app` port or use case and map the
  result. No SQL in `qd-api`.
- Routes: `auth/login|logout|me|step-up`, `status`, `decisions`,
  `decisions/{id}`, `journal`, `halts` (list, create), `halts/{id}/rearm`,
  `strategies`, `strategies/{id}/events`, `instruments`,
  `instruments/{id}/bars`, `backtests`, `account/live-armed`, `events` (SSE),
  `openapi.yaml`.
- **OpenAPI is hand-written** (`crates/qd-api/openapi.yaml`) rather than
  generated. The route list is small and a generator adds a large dependency.
  How to change: adopt `utoipa` if the route count grows past what is easy to
  keep in sync by hand.
- **Decisions are shown post-risk (INV-17):** the list and detail build
  their headline from the stored `DecisionOutcome`, never from the raw
  signal. A risk-blocked signal reads "NO TRADE" with the typed reason code
  and detail.
- **Errors** map to status codes with a stable `code`. Internal errors log
  their detail on the server and show "internal error" to the client.
- **Live updates** use Server-Sent Events. The server polls the journal every
  two seconds and sends new sequence numbers and kinds; the client re-fetches
  what it shows. `Last-Event-ID` resumes. Polling is enough for daily-bar
  trading, so there is no LISTEN/NOTIFY or Redis yet.

### Authentication and authorization (security sections missing)

- **Users:** one owner (enforced by a unique partial index) and any number
  of read-only viewers. Users are created on the server host with
  `qd user create`; the password is read from standard input, never from
  arguments. There is no sign-up endpoint.
- **Passwords:** argon2id (crate defaults), at least 12 characters.
- **Sessions:** a random 256-bit token in a cookie that is `HttpOnly`,
  `SameSite=Strict` and, when the environment is production, `Secure`.
  Only the token's SHA-256 is stored. Default lifetime 12 hours
  (`session_hours` in the server config). Logout deletes the session.
- **CSRF:** every non-GET request must carry `X-Requested-With: quantdesk`.
  A cross-site form cannot set that header; together with `SameSite=Strict`
  this blocks CSRF without tokens in pages.
- **Login throttling:** five failures per username per 15 minutes, in
  memory (resets on restart; acceptable for one owner behind a firewall).
- **Step-up:** re-arming a halt, promoting or resuming a strategy and arming
  a live account need the password re-entered within the last five minutes.
  Every such action is written to the audit log with the authenticated user
  as actor.
- **Security headers:** a strict CSP (`default-src 'self'`, no inline
  scripts, `frame-ancestors 'none'`), `nosniff`, `X-Frame-Options: DENY`,
  `Referrer-Policy: no-referrer`, `Cache-Control: no-store`.
- TLS terminates at a reverse proxy in deployment (Phase 10). The server
  binds to `127.0.0.1` by default.

How to change: session length is configuration (`session_hours`, 1–168);
throttling limits and the step-up window are constants in
`crates/qd-api/src/auth.rs`.

### Halts through the API

- The owner can create a manual halt without step-up (halting is always
  safe) and re-arm any active halt with step-up.
- A startup halt from a boot whose checks failed stays active after later
  clean boots: that failure (for example an unreconciled broker position)
  needs a human to look at it. The owner clears it with a step-up re-arm.

### Frontend approach (§10 missing)

- **Stack:** Vite and strict TypeScript, no UI framework. The screens are a
  small set of tables and forms; a framework would add dependencies without
  benefit. Fonts (Geist, JetBrains Mono) are self-hosted from npm packages,
  so there is no third-party request and the CSP stays `'self'`.
- **Rendering:** a tiny DOM builder that sets `textContent` only. No
  `innerHTML` anywhere, so API data cannot inject markup.
- **Numbers:** decimals arrive as strings and are formatted with `BigInt`
  arithmetic (Indian digit grouping), never parsed into floats. Times are
  shown in IST.
- **Screens:** login; decisions list and detail (in the order the spec's UX
  gives: headline, reason, levels, economics, risk); halts with create and
  re-arm; strategy registry with stage events; research backtest; journal.
  The top bar always shows PAPER or LIVE and whether entries are halted.
- **Labels** come from one table matching §6.3 (`src/labels.ts`); there is
  no bare BUY or SELL (INV-12).
- **Serving:** in production `qd-server` serves the built `frontend/dist`
  when `frontend_dir` is set in the config, with an `index.html` fallback
  for client routes. In development Vite proxies `/api` to `127.0.0.1:8080`.
- The Stitch screens that need data the backend does not have yet
  (positions, orders, scanner, AI views) are deferred to the phases that
  build those components.

### Deferred

- Manual entries from the UI: they must go through the Decision Engine and
  Risk Gate (INV-03) and need a live or paper broker; Phase 6.
- Positions and orders views: Phase 6 (persisted positions, paper broker).
