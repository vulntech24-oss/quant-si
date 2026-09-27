# ADR 0001: Process, phase plan and missing spec sections

- Status: accepted
- Date: 2026-09-27

## Context

The spec (`docs/QUANTDESK_BUILD_SPEC.md`) asks for Phase 0 (audit, architecture,
ADRs, API contract draft, phase plan, owner decisions) and then a stop at
Checkpoint A before any production code.

When work started:

- The repository was empty: no commits, no backend, no frontend, no CI.
- The spec as provided ends at §6.6. Sections it references are missing:
  §7 (component details, including 7.3 scanner records, 7.5 AI policy,
  7.8 risk limits, 7.11 hosting/static IP), §10 (frontend), §17 (phases) and
  §20 (phase report format). It was saved verbatim as provided.
- The Stitch frontend export (static HTML + Tailwind CDN, four screens and a
  design doc) was shared as a zip. The owner said it is a visual reference
  only.
- The owner then directed: "start with coding work".

## Decision

1. The owner's direction to start coding replaces the Checkpoint A stop for
   the work that the provided sections fully specify: the pure domain core
   (§5.3 `qd-domain`, §6). Phase 0 outputs are produced in compressed form
   (this ADR, ADR 0002–0004, `docs/PROGRESS.md`).
2. Work that depends on missing sections waits for them, or proceeds only
   with its assumptions recorded in an ADR: risk limit details (§7.8), AI
   policy (§7.5), frontend integration (§10), phase exit criteria (§17).
3. Provisional phase plan, to be replaced by §17 when it is provided:

   | Phase | Scope |
   |---|---|
   | 0 | Audit and plan (compressed; done) |
   | 1 | Domain core `qd-domain`: types, formulas, rounding, state machines (done) |
   | 2 | Pure `qd-risk` (Risk Gate rules, halt triggers) and `qd-strategy` (features, regime, Strategy trait, first strategy); versioned cost model |
   | 3 | `qd-app` ports and use cases (proposal, decision, risk gate, order gateway, position manager, journal); `qd-backtest` simulated clock and broker |
   | 4 | `qd-store` (PostgreSQL with append-only enforcement, Redis), `qd-server` wiring and startup halts, `qd-cli` |
   | 5 | `qd-api` (Axum, auth, OpenAPI, SSE) and frontend integration |
   | 6 | Market data adapters and the paper broker on live quotes |
   | 7 | Validation pipeline (walk-forward, holdout, Monte Carlo) and review reports |
   | 8 | AI orchestrator (advisory, shadow logging by default) |
   | 9 | Kite live order executor behind the `live-orders` feature, only after the owner confirms §4 items 5–7 |

4. The Stitch export is kept in `design/stitch-reference/` as a visual
   reference. Its content (bare BUY/SELL, "confidence" cards, HFT metrics,
   non-Kite instruments) is not a product requirement; see its README.

## Alternatives

- Stop at Checkpoint A anyway: contradicts the owner's latest direction.
- Build every crate as scaffolding now: the spec forbids placeholders and
  asks for crate count proportional to code.

## Consequences

- The provided spec sections are implemented faithfully; anything beyond them
  is flagged in ADRs and `docs/PROGRESS.md`.
- The owner should add §7–§20 to `docs/QUANTDESK_BUILD_SPEC.md` before Phase 2
  (risk limits) and Phase 5 (frontend).
