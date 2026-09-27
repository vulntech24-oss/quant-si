# ADR 0015: Data quality, more strategies, portfolio view, security and operations upgrades

- Status: accepted
- Date: 2026-09-27

## Context

The owner asked for the suggested upgrades:
- data in the UI, with quality checks;
- more strategies;
- a portfolio view and charts;
- phone alerts;
- TOTP and sessions;
- settings history;
- key rotation;
- off-site backups;
- holiday calendars;
- a parameter search;
- a mobile layout.

Kite login in the UI and the automatic daily import already existed
(ADR 0014).

## Decisions

### Holiday calendars and data quality

**Calendars** (`qd_domain::calendar`, `config/calendars/india.toml`)
- Calendars are data: weekends, weekday holidays, special sessions
  (Muhurat) and a coverage range. Outside the coverage the answer is
  "unknown" and callers fall back to calendar-day rules. Nothing is
  guessed.
- The 2026 NSE/BSE/NFO and MCX lists come from Zerodha's holiday calendar;
  NSE's and MCX's own pages refuse automated reads.
- Only full MCX closures are listed: on most NSE holidays MCX still trades
  in the evening.
- Add next year's list as a new version when the exchanges publish it.

**Checks** (`check_bars`)
- Flagged: missing trading days, bars on holidays, and close-to-close
  jumps above `data.max_close_jump` (20% by default: Indian circuit limits
  stop most stocks there, so a larger move is usually a split, a bonus or
  a bad print).
- **Kite import:** a jump holds back that bar and every later one
  (`data.hold_suspect_bars`). Telegram names the instruments with issues.
  The owner reviews them and imports adjusted data.
- **CSV upload:** a jump refuses the import unless "accept jumps" is ticked
  (audited with the count).
- **Stale alert:** it counts unprocessed trading days (two or more) when
  the calendar covers them.
- **Fill checks:** they skip exchange holidays.

### Instruments and bars in the web UI

- The `DataAdmin` port and the Data page add spec versions from TOML
  (specs stay immutable), upload CSVs and check stored bars. The
  validation is the same as the CLI's.

### Strategies

- `breakout-1.0.0`: long a close above the prior 20-day high and the
  50-day average, at most 1 ATR beyond (not chased), in TrendUp or Range.
- `mean-reversion-1.0.0`: long a close 1.5 ATR under the 20-day average
  but above the prior 20-day low, in Range only; the target is the
  average.
- `trend-pullback-short-1.0.0`: the mirror of `trend-pullback`, only on
  instruments that allow an overnight short. Elsewhere it has no setup,
  which avoids NO TRADE noise.
- The catalog carries each version's RR floor. There is a generic
  `qd strategy register --logic-version`, and research backtests take a
  logic version.
- None of them has evidence yet. Each needs validation on real data like
  the first one.

### Portfolio view and decision charts

- **Portfolio** (`qd_app::book_view`), from the journal only:
  - the equity curve and drawdown;
  - open exposure by correlation bucket and asset class (notional and
    risk to the stops);
  - P&L per strategy version.
- The sector is not in instrument specs, so the correlation bucket stands
  in for it.
- **Decision charts:** a candle chart around the decision with entry,
  stop and target. Chart coordinates use floats, as the existing sparkline
  does; labels keep the exact decimal strings.

### Security

**TOTP (RFC 6238)**
- The algorithm is SHA-1, 30-second steps, 6 digits and ±1 step of drift,
  tested against the RFC vectors.
- The secret is encrypted with the master key (associated data
  `totp:<user>`).
- Every accepted code consumes its step, so a code cannot be replayed.
- Enrolling and removing need the owner, a step-up and a current code.
- With TOTP on, login needs the code. A missing or wrong code counts
  toward the login throttle.
- `qd user reset-totp` on the server recovers a lost authenticator.

**Sessions**
- The list shows ids made from the first 12 hex digits of the token hash,
  never the token.
- One session can be revoked, or all of them ("log out everywhere", this
  one included).

**Settings history**
- Every version is listed with the fields that differ from now.
- Restore validates the old value again and saves it as a new version.
  History is never rewritten.

**Master-key rotation**
- The sequence:
  1. write `master.key.new`;
  2. re-encrypt every API key and TOTP secret in one transaction;
  3. rename the files (`master.key.previous` is kept).
- On restart, a crash after the commit is finished, and an uncommitted
  rotation is discarded. If neither key opens the data, startup refuses.
- A key from `QD_MASTER_KEY` is rotated by the operator, not from the UI.

### Operations

**Backups** (`deploy/backup.sh`)
- A dump plus a copy of the master key.
- With `QD_BACKUP_PASSPHRASE`, both are encrypted with AES-256-CBC and
  PBKDF2 (600,000 iterations).
- With `QD_BACKUP_REMOTE`, they go off-site through rclone: the dump
  under `db/`, the key under `keys/`, or under a separate
  `QD_BACKUP_KEY_REMOTE`.
- Unencrypted backups are never copied off-site.

**Restore test** (`deploy/restore-test.sh`, monthly by cron)
- Restores the newest dump into a scratch database.
- Checks that the history tables are present and never ahead of the live
  database, and that the backed-up key matches the key in use.
- The exit status tells cron whether it passed.

**Alerts** add live fills (one line per fill) to critical alerts and
daily results. Email is not built: Telegram covers the phone, and email
needs a provider choice.

### Parameter search (research only)

- Grids have at most 12 candidates around each catalog version.
- **Procedure:** for each quarter, choose the best mean R over the
  preceding year (minimum 10 trades), then run it on the unseen quarter.
- **Report:**
  - the out-of-sample result of that procedure;
  - the in-sample figure it chose on (the gap shows overfitting);
  - the catalog parameters on the same quarters;
  - trials per window;
  - a verdict.
- It is never evidence and cannot be registered. A better set becomes a
  new logic version in code and goes through validation and paper trading
  (INV-10).

### Mobile

- Under 800px:
  - the navigation folds behind a Menu button;
  - tables scroll sideways;
  - controls are touch-sized;
  - a fixed bar keeps Decisions, Portfolio and the Kill switch one tap
    away.
- Halting needs no step-up (it reduces risk), so it is quick from a
  phone.

## Consequences

New migration `…05_totp.sql`. New settings section `data`. New
server-file field `calendars` (the default is the shipped file). New
dependencies `hmac` and `sha1` (RustCrypto, MIT/Apache).
