# ADR 0002: Workspace layout and lint policy

- Status: accepted
- Date: 2026-09-27

## Context

§5.3 proposes a Cargo workspace under `backend/` with thirteen crates and asks
to keep the crate count proportional to the code. §1.4 and §1.7 set the
verification commands and ban placeholder macros.

## Decision

- The workspace lives in `backend/` (edition 2024, MSRV 1.85, resolver 3).
  Crates are added when they get real code. Phase 1 has one: `qd-domain`.
  The §5.3 names and dependency rules stay the target layout.
- Shared dependency versions and lints live in `backend/Cargo.toml`
  (`[workspace.dependencies]`, `[workspace.lints]`).
- Workspace lints: `unsafe_code = forbid`; `clippy::todo`,
  `clippy::unimplemented`, `clippy::dbg_macro`, `clippy::unwrap_used` and
  `clippy::expect_used` denied. Integration-test files allow unwrap/expect at
  file level with a comment (a failed unwrap is a failed test).
- `qd-domain` additionally denies `clippy::float_arithmetic`, `float_cmp`,
  `print_stdout` and `print_stderr`, and its `clippy.toml` bans clock reads
  (`Utc::now`, `Local::now`, `SystemTime::now`, `Instant::now`,
  `Uuid::now_v7`) so "time is always passed in" (§5.4) is checked by clippy.
  `chrono` is built without its `clock` feature in this crate.
- `cargo deny` (`backend/deny.toml`) checks advisories, licenses (explicit
  allow-list of the licenses in use), bans (no wildcards) and sources
  (crates.io only).
- `Cargo.lock` is committed so builds are reproducible.

## Alternatives

- Create all thirteen crates now: empty crates are placeholders.
- Put the workspace at the repository root: `backend/` keeps the Rust code
  separate from the frontend and docs, as §5.3 shows.

## Consequences

- New crates must opt in with `[lints] workspace = true`.
- Pure crates (`qd-strategy`, `qd-risk`) should copy `qd-domain`'s clock ban.
