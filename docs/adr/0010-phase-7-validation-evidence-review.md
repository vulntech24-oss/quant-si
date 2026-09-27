# ADR 0010: Phase 7 decisions: validation, evidence, Monte Carlo, review

- Status: accepted
- Date: 2026-09-27

## Context

INV-11 requires owner approval backed by evidence for every promotion. The
Decision Engine needs outcome probabilities from "validated evidence" with at
least `min_evidence` comparable out-of-sample setups (§4). The spec sections
that would define the validation protocol and the review (§7 onwards, §17)
are missing, so this ADR defines them. The goal is to be conservative and
simple.

## Decisions

### Evidence is a recorded, append-only fact

- New table `evidence_records`: id, version, kind (`validation` or
  `paper_review`), passed, full report, time. It is append-only by trigger
  (INV-16), so a verdict is never edited after the fact.
- **The registry checks cited evidence, not just its presence** (INV-11):
  - `PassResearch` and the promotion to Paper need a **passed validation of
    this version**;
  - promotions to SmallCapital and Full need a **passed paper review of this
    version**.
  Unrecorded, failed, wrong-kind and other-version evidence is refused.
  **Until a paper review passes, no version can reach a live stage.** Even
  then, live orders still need every INV-14 condition.
- The frontend no longer asks for an evidence UUID. It cites the latest
  passed record of the right kind and asks the owner to confirm, and the
  step-up is still required.

### Validation protocol (`qd-backtest::validation`)

Strategy parameters are fixed by the logic version (INV-10); nothing is
fitted. The protocol is:

1. **Split** `[from, to]` into a development period and a **holdout** (the
   last `holdout_fraction`, default 20%).
2. **Walk forward** over the development period in consecutive windows
   (`window_days`, default 91). Each window is an independent run from a
   flat account, using point-in-time series (INV-09) with earlier bars as
   indicator history. Positions open at a window's end are not counted.
3. **Holdout:** one run after the windows, as the final check.
4. **Evidence tables** come from the out-of-sample (window) trades only,
   per setup type:
   - `p_target` is the share of trades that hit the target, rounded **down**
     to four places;
   - `p_stop` is the share that hit the stop, rounded **up**;
   - `p_time` is the remainder, so the three sum to exactly 1;
   - `time_exit_r` is the mean net R of the trades that ended any other way.
   The holdout never feeds the tables, so it stays an independent check.
5. **Monte Carlo** (`qd-backtest::montecarlo`) bootstraps the out-of-sample
   R-multiples at the configured `risk_per_trade`, using seeded SplitMix64,
   so a report reproduces exactly. It reports drawdown percentiles, final
   return percentiles and the share of paths that reach the hard-halt
   drawdown. Floats appear only inside these statistics (spec §6.1).
6. **Checks**, all of which must pass (`config/validation.toml`):
   - minimum out-of-sample trades (30, the same as `min_evidence`);
   - minimum out-of-sample expectancy (0.10R);
   - share of positive windows (≥ 50%);
   - worst window drawdown (≤ 15%);
   - minimum holdout trades (5) and holdout expectancy (≥ 0R);
   - Monte Carlo p95 drawdown (≤ 20%) and share of paths reaching the
     hard-halt drawdown (≤ 5%).
   Every check is recorded with its measured value.
- Validation runs use the research configuration (neutral evidence prior,
  minimum EV 0) and simulate the version at the Paper stage (ADR 0006).
  They measure what the setups do; they do not assume it.
- `StoreValidator` validates a registered version over stored bars:
  - it refuses a version whose logic version or parameters are not in the
    build (INV-10);
  - it records the evidence, passed or failed, and audits it.
  It is exposed as `POST /api/validations`, `qd strategy validate`, and
  `GET /api/evidence` for the list.

### Evidence for paper decisions

The paper runner loads evidence at the start of each run
(`StoreEvidenceLoader`). It uses the tables of each version's **latest
passed validation**, with the source recorded as `oos-empirical-v1`. The
Decision Engine's evidence gate still requires `count ≥ min_evidence`.

### Review and calibration (`qd-app::review`)

- Joins each journaled entry decision with its closed trade (decision ids
  are now carried on positions and trades). Per version it reports:
  - predicted versus realized outcome shares;
  - the Brier score of `p_target`;
  - calibration bins of width 0.2;
  - predicted EV in R versus realized mean R.
  Per account it reports the maximum drawdown and the number of days with
  reconciliation mismatches or unprotected positions ("operational
  incidents"). All arithmetic is decimal.
- A **paper review** applies `config/review.toml`:
  - at least 30 trades and 60 paper days;
  - realized expectancy ≥ 0.05R;
  - realized mean R trails predicted EV by at most 0.30R;
  - account drawdown ≤ 10%;
  - Brier score ≤ 0.25;
  - no operational incidents.
  The review is recorded as `paper_review` evidence, pass or fail. It is
  exposed as `GET /api/review`, `POST /api/review/{version}/record` (owner),
  and `qd strategy review [--record]`.

## Defaults and how to change them

All thresholds live in `backend/config/validation.toml` and
`backend/config/review.toml`, and the server config can point elsewhere
(`validation_criteria`, `review_criteria`). They are starting points chosen
to fail closed. The owner should review them against the chosen universe
before relying on them.

## Consequences

- Automatic demotion on a live breach (INV-11, second half) belongs with
  live trading (Phase 9). The review already measures what such a breach
  rule would use.
- Evidence tables per setup type pool all instruments of a validation run.
  Per-instrument or per-regime tables would need more data than daily bars
  usually give. Revisit with real data.
