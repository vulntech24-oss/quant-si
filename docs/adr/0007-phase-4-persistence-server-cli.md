# ADR 0007: Phase 4 decisions: persistence, server and CLI

- Status: accepted
- Date: 2026-09-27

## Context

Phase 4 adds PostgreSQL storage (`qd-store`), the server binary (`qd-server`)
and the admin CLI (`qd-cli`). The spec names Tokio, Axum, SQLx, PostgreSQL and
Redis but gives no schema, configuration format or startup detail (§7, §17
missing).

## Decisions

### Storage

- One migration set in `backend/migrations`, embedded in the binaries and
  applied by `qd migrate` or at server start.
- **Append-only history in the database (INV-16):** `journal`,
  `halt_events`, `instrument_specs`, `bars`, `strategy_versions`,
  `strategy_stage_events` and `audit_log` carry row triggers that reject
  UPDATE and DELETE and statement triggers that reject TRUNCATE. `accounts`
  is the only mutable table; every change to it is written to `audit_log`
  in the same transaction, and a constraint prevents arming a non-live account.
- **Halts** are stored as versions (created, then cleared); loading takes the
  latest version of each and re-validates it through the domain constructors,
  so a tampered row fails closed (INV-06, INV-07).
- **Bars are point-in-time (INV-09):** corrections are new rows with a later
  `ingested_at`; reads take the latest row ingested at or before the
  snapshot time.
- **Strategy registry:** versions are immutable rows; the stage is the replay
  of stored stage events through the domain state machine, so an illegal
  transition cannot be stored. Concurrent transitions are not expected (one
  owner); if they become possible, add an expected-sequence check.
- **Queries** are runtime-checked `sqlx` queries, not compile-time macros, so
  the build does not need a database or `.sqlx` offline data; every query is
  exercised by integration tests against a real PostgreSQL. `cargo sqlx
  prepare --check` is therefore not applicable (spec §1.4 makes it
  conditional on query macros existing).
- **Tests need PostgreSQL:** `DATABASE_URL` must point at a server where the
  user may create databases (`#[sqlx::test]` creates one per test). CI runs
  a `postgres:16` service. `cargo test --workspace` fails without it, on purpose.
- **TLS to the database** is not enabled: the database runs on the same host
  or private Docker network (Phase 10 deployment). Revisit before any remote
  database.
- **Redis is deferred.** Nothing needs it yet: PostgreSQL is the source of
  truth and the daily-bar loop has no hot cache or fan-out. It will be added
  if live quote fan-out needs it (Phase 6).

### Server

- Non-secret settings in a TOML file (`QD_CONFIG`, default
  `config/quantdesk.toml`; example in `backend/config/quantdesk.example.toml`).
  The database URL comes only from `QD_DATABASE_URL` and is held in a
  `Secret` whose `Debug` output is redacted (INV-15).
- **Defaults are safe:** development environment, live trading off, bind
  `127.0.0.1:8080`.
- **The server refuses to start** when live trading is enabled outside
  production, without the `live-orders` build, or with any unverified cost
  schedule.
- **Startup (INV-07):** a global `Startup` halt is recorded first; database
  and halt-store checks and broker reconciliation follow; the system clears
  the startup halt only if all pass. With no broker configured (the case
  until Phase 6) entries stay halted. Earlier hard and manual halts are untouched.
- `/health` (liveness) and `/ready` (checks, active halts, whether entries
  are halted; an unreadable halt store reports halted).
- Logs are JSON via `tracing`; secrets are never logged.

### CLI

- `qd` connects with `QD_DATABASE_URL` and uses the same stores and use
  cases as the server, so every change is audited and validated:
  `migrate`, `account create`, `instrument add|list`, `bars import`
  (CSV, validated and ordered before anything is written), `strategy
  register-trend-pullback|list|event`, `halt create|clear|list`,
  `journal`, and a research `backtest` on stored bars.
- The CLI is an owner tool on the server host. Promotions through it still
  need an `OwnerApproval` with evidence in the event JSON; the step-up
  authentication for promotions and live arming belongs to the API (Phase 5).

## Consequences

- Phase 5 adds `qd-api` (auth, decision and journal views, halts, manual
  entries through the Decision Engine) on this server.
- Phase 6 wires broker adapters, which lets startup reconciliation pass.
