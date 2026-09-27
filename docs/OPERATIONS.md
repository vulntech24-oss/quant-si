# QuantDesk operator guide

For the owner running QuantDesk on one server. Paper trading is the
default; live trading at Zerodha is off until you complete §6a.

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
- **Paper trading, risk limits, advisory AI (with the OpenAI, Gemini and xAI
  switches), Zerodha Kite, notifications, validation and review criteria**:
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
- Daily bars come from Zerodha:
  - give each instrument spec a broker reference: `broker = "kite"`,
    `symbol` = the Kite trading symbol, and optionally `token`;
  - switch Zerodha on and log in (§6a);
  - bars are imported daily at `sync_bars_utc`, or with **Broker → Import
    daily bars now**. Only completed days are imported.
- CSV import still works: `qd bars import --instrument <id> bars.csv`
  (`docs/integrations/csv-bars.md`). A corrected bar is a new row, and
  earlier decisions keep the data they saw.
- Cost schedules (`backend/config/costs/india-zerodha.toml`) were checked
  against zerodha.com/charges on 2026-09-27. When Zerodha or the exchanges
  change a rate, add a new schedule version with the new `checked_on` date;
  live trading refuses unverified schedules.

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
  condition in §6a.
- While a hard halt (drawdown) is active, live versions are demoted to
  Paper automatically before and after every live run (INV-11). Promoting
  them again needs a new passed review and your approval.

## 6a. Zerodha and live trading

**Once, at Zerodha:**
- Subscribe to Kite Connect ("Connect", which includes historical data).
- In the developer console, set the redirect URL to
  `https://<your domain>/api/kite/callback`.
- Register your server's static IP under "IP Whitelist". SEBI requires it
  for order endpoints.
- Set up DDPI (or TPIN) so delivery sells go through, and TOTP on your
  account.

**Once, in QuantDesk:**
1. Settings → API keys: enter the Kite API key and secret.
2. Settings → Zerodha Kite: your client id (for example AB1234), then
   switch the connection on.
3. **Broker → Login with Zerodha**: this logs in to Zerodha and brings you
   back. Log in again every morning, because sessions end at 06:00 IST.

**Live book:** configured in the server file only (never in the web UI).
1. Create a live account:
   `qd account create --name live --mode live --id <uuid>`.
2. Add `[live]` to `quantdesk.toml` (see the example) with that account,
   your capital, a start date and `daily_run_utc`.
3. Real orders also need all of these:
   - a build with the `live-orders` feature
     (`cargo build --features qd-server/live-orders`);
   - `environment = "production"` and `live_trading_enabled = true`;
   - **Broker → Arm live account** (asks for your password);
   - a strategy version at SmallCapital.

The **Broker** page shows every condition. Before real money, place one
tiny order yourself and check that the journal, the book and Kite agree.

**What runs by itself:**
- **Bar import** at `sync_bars_utc`.
- **Live cycle** at `[live] daily_run_utc`, for the latest completed day
  only.
- **Fill checks** every `sync_fills_minutes` during market hours: a filled
  entry gets its stop and target at once, as one GTT at Zerodha.

**What stops trading:**
- A book that disagrees with Zerodha, an order Zerodha cannot account for,
  or a restart without today's login halts new entries. Exits and stops
  still go out.
- Check Kite, then re-arm on the **Kill switch** page.

## 6b. Telegram

1. Create a bot with @BotFather and put its token in Settings → API keys.
2. Message the bot, find your chat id, and enter it in Settings →
   Notifications.
3. **Broker → Send a test Telegram message**, then switch notifications on.

What you get: critical alerts (or warnings too), daily run summaries, and
job failures (once a day each).

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
  - the paper or live book is inconsistent;
  - there are unprotected positions (paper or live);
  - the live book is unreadable.
- Alerts also go to Telegram when notifications are on (§6b). Live gauges:
  `qd_live_book_consistent`, `qd_live_open_positions` and
  `qd_live_unprotected_positions`.
  Warnings: other halts, a stale paper book (more than 4 days), paper not
  started.

## 10. Upgrades

```sh
git pull && cd deploy && docker compose build && docker compose up -d
```

Migrations run at start and only ever add. After an upgrade, startup keeps
entries halted until reconciliation passes, as on every start.

## 11. Security notes

- Secrets live in `deploy/.env` on the server (`QD_POSTGRES_PASSWORD`,
  optionally `QD_MASTER_KEY`). Provider keys and tokens are entered in
  Settings and stored encrypted. They are never in files in the repository,
  never in logs, and never sent to the browser.
- The Zerodha callback needs no session: a one-time state (10 minutes, one
  use) proves you started the login. A login as any other client id is
  refused.
- The owner and viewers log in with argon2id passwords (at least 12
  characters).
  - Sessions are `HttpOnly`, `SameSite=Strict`, and `Secure` in production.
  - Five failed logins per 15 minutes lock a username.
  - Dangerous actions need the password again within 5 minutes.
- Strict CSP, `X-Frame-Options: DENY`, HSTS at the proxy, and a CSRF header
  on every change.
