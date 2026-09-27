# QuantDesk operator guide

For the owner running QuantDesk on one server. Paper trading only until the
live broker adapter exists (see `docs/integrations/kite.md`).

## 1. Install (Docker, one host)

Requirements: Docker with the compose plugin, a domain name pointing at the
host for TLS (or `localhost` for a private test), and ports 80 and 443 open.

```sh
git clone <repo> quantdesk && cd quantdesk/deploy
cp .env.example .env        # then edit .env on the server, never commit it
#   QD_POSTGRES_PASSWORD=<a long random value you generate>
#   QD_DOMAIN=<your domain>
docker compose build
docker compose up -d
```

- Behind a TLS-intercepting corporate proxy, build with
  `docker build --build-arg HTTPS_PROXY=... --secret id=ca_bundle,src=<proxy CA> .`
  Neither the proxy nor the CA stays in the image.
- Only the Caddy proxy publishes ports. `qd-server` listens on the internal
  network, and `/metrics` is not served through the proxy.
- The container runs read-only, without capabilities, as UID 10001.

## 2. First run

```sh
docker compose exec qd-server qd account create --name paper --mode paper \
    --id 00000000-0000-7000-8000-000000000001      # the id in quantdesk.toml
echo '<a long owner password>' | docker compose exec -T qd-server qd user create --username owner --role owner
docker compose restart qd-server
```

- Type the password on the server itself; it is read from standard input and
  never from arguments.
- On restart, startup reconciliation passes and this boot's startup halt
  clears. Halts left by earlier boots whose checks failed stay until you
  re-arm them. Open `https://<domain>`, log in, and go to **Kill switch** to
  review and re-arm them; re-arming needs your password again.
- Edit the configuration by mounting your own `deploy/quantdesk.toml` (see
  the commented `volumes` entry in `compose.yml`).

## 2a. Settings page (no file editing)

Open **Settings** in the web UI:

- **API keys and credentials** (Zerodha Kite, OpenAI, Gemini, xAI, crypto):
  paste a value and press Save.
  - Keys are encrypted on the server and never shown again; the page only
    shows whether a key is set.
  - Saving or deleting asks for your password.
  - The master key that encrypts them is created automatically in the
    `qdstate` volume. Back it up (`docker compose cp qd-server:/var/lib/quantdesk/master.key .`)
    and keep it apart from the database backups.
- **Paper trading, risk limits, advisory AI, validation and review criteria**:
  edit the fields and press Save.
  - Each change is validated, needs your password, applies from the next
    run without a restart, and is kept in history with who changed what.
  - "Reset to file default" undoes your overrides.
- **Not on the page, on purpose:** live trading on or off, the environment,
  the account and the database. They stay in the server configuration, so no
  browser session can turn on real money.

## 3. Instruments and data

- Add instrument specs with `qd instrument add <spec.toml>`. See
  `backend/config/instruments/examples/`.
- Daily bars come from CSV: `qd bars import --instrument <id> bars.csv`
  (`docs/integrations/csv-bars.md`). A corrected bar is a new row, and
  earlier decisions keep the data they saw.
- **Cost schedules are unverified** (`backend/config/costs/india-zerodha.toml`).
  Check every rate against zerodha.com/charges and mark schedules verified
  before relying on costs. Live trading refuses unverified schedules.

## 4. From a strategy version to paper trading

1. Register a version: `qd strategy register-trend-pullback --version-number 1 --git-sha <sha>`.
2. Start research: **Strategies → Start research**.
3. Validate: **Validation**. Choose the version, instruments and date range.
   The run covers walk-forward windows, a holdout run once at the end, and
   Monte Carlo. Every result is recorded, pass or fail.
4. **Pass research** and **Promote to Paper**. Both cite the latest *passed*
   validation automatically and need your password.
5. Paper decisions now use that validation's evidence tables. Without them,
   every decision is NO TRADE with `insufficient_evidence`.

## 5. Daily routine

1. After the close, import the day's bars.
2. Paper trading runs automatically at `[paper] daily_run_utc`, or on demand
   from **Paper → Run** or `qd paper run --config <file>`. Missed days are
   processed in order, and a day is never processed twice.
3. Look at **Decisions**. The headline is always post-risk; NO TRADE shows
   its reason.
4. If enabled, advisory AI comments on new entries (**AI** page). It never
   changes anything.
5. Alerts appear under the top bar and in the JSON logs ("alert raised").

## 6. Review and live stages

- **Review** compares predicted and realized outcomes (calibration bins,
  Brier score, EV versus realized R, drawdown, operational incidents).
- **Record paper review** stores the verdict. Only a passed review can back a
  promotion to SmallCapital or Full. Even then, live orders need every
  condition in the README, and **no live broker adapter exists yet**.

## 7. Kill switch

- **Halt new entries** (Kill switch page) stops entries at once. Exits and
  protective orders always continue.
- Hard halts (drawdown) and manual halts need your password to re-arm.
- If the paper book cannot be restored consistently, an operational halt is
  recorded and paper runs are refused until you investigate. Nothing
  auto-repairs the book.

## 8. Backups and restore

```sh
cd deploy && ./backup.sh backups      # pg_dump, custom format; keeps 30 days
# restore into an empty database:
docker compose exec -T postgres pg_restore -U quantdesk -d quantdesk --clean --if-exists < backups/<file>.dump
```

Schedule `backup.sh` daily (cron) and copy the files off the host. The
history tables are append-only, so a dump is the complete record.

## 9. Monitoring

- `GET /health`: liveness. `GET /ready`: database, halt store, active halts.
- `GET /metrics` (internal network only): Prometheus gauges, including
  - `qd_entries_halted` and `qd_active_halts`;
  - `qd_alerts_critical`;
  - `qd_paper_book_consistent` and `qd_paper_last_day_age_days`;
  - `qd_paper_unprotected_positions`.
- Critical alerts:
  - the kill switch is unreadable;
  - a hard halt is active;
  - the paper book is inconsistent;
  - there are unprotected positions.
  Warnings: other halts, a stale paper book (more than 4 days), paper not
  started.

## 10. Upgrades

```sh
git pull && cd deploy && docker compose build && docker compose up -d
```

Migrations run at start and only ever add. After an upgrade, startup keeps
entries halted until reconciliation passes, as on every start.

## 11. Security notes

- Secrets live only in `deploy/.env` on the server (`QD_POSTGRES_PASSWORD`)
  and later in provider keys (`QD_KITE_*`, `QD_OPENAI_API_KEY`, ...). They
  are never in files in the repository, never in logs, and never sent to the
  browser.
- The owner and viewers log in with argon2id passwords (at least 12
  characters).
  - Sessions are `HttpOnly`, `SameSite=Strict`, and `Secure` in production.
  - Five failed logins per 15 minutes lock a username.
  - Dangerous actions need the password again within 5 minutes.
- Strict CSP, `X-Frame-Options: DENY`, HSTS at the proxy, and a CSRF header
  on every change.
