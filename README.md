# QuantDesk

A quantitative trading-intelligence platform for one owner: analyze → find
opportunity → predict → decide → validate → manage risk → execute → monitor →
learn. It makes defined-risk trades lasting days to weeks on completed daily
bars. **Paper trading is the default everywhere, and NO TRADE is a normal,
frequent outcome.**

Rust modular monolith (Tokio, Axum, SQLx, PostgreSQL) with a small strict
TypeScript frontend.

## What it does today

| Area | State |
|---|---|
| Domain core, Risk Gate, kill switch, Order Gateway, Position Manager | Built, tested (17 invariants) |
| Strategy `trend-pullback-1.0.0`, registry with immutable versions | Built |
| Backtesting, walk-forward / out-of-sample / holdout validation, Monte Carlo | Built |
| Paper trading (daily cycle shared with backtests, restore from the journal) | Built |
| Evidence-gated promotions; review and calibration | Built |
| Advisory AI (shadow mode) with a deterministic checklist advisor | Built |
| HTTP API, owner authentication, frontend | Built |
| Settings page: all settings and API keys from the web UI (keys encrypted, write-only) | Built |
| Docker image, compose stack with TLS, backups, metrics, alerts | Built |
| Zerodha Kite: daily login, bar import, account sync, live orders (GTT stop/target), live runner | Built (tested against a fake Kite; see ADR 0014 before real money) |
| Automatic demotion of live strategies on a hard halt | Built |
| Hosted AI advisors: OpenAI, Gemini, xAI (advisory only) | Built |
| Telegram alerts, live fills and daily summaries | Built |
| Holiday calendars (NSE/BSE/MCX 2026), data-quality checks, Data page (instruments, CSV upload) | Built |
| Strategies: trend pullback (long and short), breakout, mean reversion; parameter search (research) | Built |
| Portfolio page, decision charts, phone layout with a one-tap kill switch | Built |
| TOTP login, session management, settings history, master-key rotation, encrypted off-site backups | Built |
| Crypto venue | **Not built**: Binance refuses this region (HTTP 451); the venue is an owner decision |

Real money is disabled by default and cannot be enabled by configuration
alone. Live orders need all of these (INV-14):
- a build with the `live-orders` feature;
- `environment = "production"`;
- `live_trading_enabled = true`;
- verified cost schedules;
- a `[live]` book in the server file;
- an armed live account (password step-up);
- a strategy version at a live stage, which needs a passed paper review;
- today's "Login with Zerodha" session.

## Quick start (development)

```sh
# PostgreSQL 16 reachable, then:
cd backend
export QD_DATABASE_URL=postgres://USER@localhost:5432/quantdesk
cargo run -p qd-cli -- migrate
cargo run -p qd-cli -- account create --name paper --mode paper --id 00000000-0000-7000-8000-000000000001
echo 'a long owner password' | cargo run -p qd-cli -- user create --username owner --role owner
cp config/quantdesk.example.toml config/quantdesk.toml
QD_CONFIG=config/quantdesk.toml cargo run -p qd-server

cd ../frontend && npm ci && npm run dev   # http://localhost:5173, proxies /api
```

Deployment with Docker and TLS, the daily routine, and the path from
validation to promotion are in [docs/OPERATIONS.md](docs/OPERATIONS.md).

## Documentation

- [docs/QUANTDESK_BUILD_SPEC.md](docs/QUANTDESK_BUILD_SPEC.md): the specification (source of truth; §7–§20 missing).
- [docs/adr/](docs/adr/): every decision and assumption, with defaults and how to change them.
- [docs/PROGRESS.md](docs/PROGRESS.md): phase log, verification and open issues.
- [docs/OPERATIONS.md](docs/OPERATIONS.md): operator guide.
- [docs/integrations/](docs/integrations/): provider status.
- [CLAUDE.md](CLAUDE.md): code map, commands, rules and invariants.
