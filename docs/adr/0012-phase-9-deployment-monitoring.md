# ADR 0012: Phase 9 decisions: deployment, monitoring, alerts

- Status: accepted
- Date: 2026-09-27

## Context

The spec asks for Docker/deployment, monitoring and health, and security,
with no Kubernetes or microservices. The spec's sections on operations are
missing. The live broker path cannot be built: the Kite documentation is
unreachable (ADR 0009). This phase makes the paper system deployable and
observable, and keeps real money impossible by default.

## Decisions

### One image, one host

- **Multi-stage Dockerfile:**
  - frontend build (node 22);
  - release build of `qd-server` and `qd` (rust 1.94, `--locked`);
  - a `debian:bookworm-slim` runtime with **no package-manager step**: CA
    certificates are copied from the build stage, and the healthcheck is
    built in (`qd-server healthcheck`).
  The image runs as UID 10001. It carries the built frontend and the data
  configuration (risk, costs, validation, review) in `/etc/quantdesk`.
  Size: about 146 MB.
- **`live-orders` is not compiled in.** It can be added with
  `--build-arg FEATURES=qd-app/live-orders`; INV-14 still applies at runtime.
- **Compose stack** (`deploy/compose.yml`):
  - `postgres:16` with a named volume;
  - `qd-server`: read-only root filesystem, all capabilities dropped,
    `no-new-privileges`, no published port;
  - Caddy 2: automatic TLS, HSTS, and the only published ports.
  Secrets come from `deploy/.env` (git-ignored; `.env.example` has names only).
- **Proxy support at build time:** the proxy is passed as `HTTPS_PROXY`
  build args and its CA as a BuildKit secret, so neither is stored in the image.
- **Container configuration** (`deploy/quantdesk.toml`): `environment =
  "production"` (Secure cookies behind TLS), bind `0.0.0.0:8080` on the
  internal network, live trading off, paper runs daily at 13:00 UTC, AI off.
- **Backups:** `deploy/backup.sh` (pg_dump custom format, 30-day retention).
  Restore with `pg_restore`. The history tables are append-only, so a dump is
  the complete record.

### Fail closed at startup: account check

Startup reconciliation with the paper venue now also requires that the
configured account **exists and is a paper account**. A fresh deployment
therefore keeps entries halted until the owner creates the account.
`qd account create --id` creates it with the id in the configuration.

### Monitoring and alerts (`qd-app::monitor`)

- `collect()` reads the kill switch and the paper book, and derives the
  alerts below:

  | Severity | Code | Condition |
  |---|---|---|
  | Critical | `halt_state_unknown` | The halt store cannot be read (entries halted, INV-06) |
  | Critical | `entries_halted` | A hard halt is active |
  | Critical | `paper_book_inconsistent` | The paper book does not restore consistently |
  | Critical | `unprotected_positions` | A position has no valid protective stop |
  | Critical | `paper_state_unreadable` | The paper book cannot be read |
  | Warning | `entries_halted` | Any other halt is active |
  | Warning | `paper_stale` | The last processed day is more than 4 days old |
  | Warning | `paper_not_started` | No paper day has been processed |

- Exposed three ways:
  - `/metrics` (Prometheus text, internal only; the proxy returns 404);
  - `alerts` and `paper` in `/api/status`, shown as a banner in the UI;
  - a background task that logs alert changes every 5 minutes as JSON
    events: raised critical at error level, warning at warn, cleared at info.
- **External notification channels** (email, chat) need provider docs and
  owner choices, so they are not built. The JSON logs are the integration
  point for a log shipper.

### CI

A `docker` job builds the image on every push, and checks that it runs as
UID 10001 and carries no credential in its environment.

## Verified here

- The image was built.
- The compose stack came up:
  - TLS through Caddy; HSTS and CSP present; `/metrics` returns 404 through
    the proxy;
  - the healthcheck is healthy;
  - startup refused to clear the halt while the account was missing;
  - after `qd account create --id` and `qd user create`, reconciliation passed;
  - login over HTTPS returned a `Secure; HttpOnly; SameSite=Strict` cookie,
    and the password never appeared in logs.
- The stack was then removed.

## Not done

- The live broker executor and account reader (Kite), because its docs are
  unreachable.
- Automatic demotion on a live breach (INV-11) goes with the live runner.
  The review already measures the inputs such a rule needs.
- Notifications beyond logs.
