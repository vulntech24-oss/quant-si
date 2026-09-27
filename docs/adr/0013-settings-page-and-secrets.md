# ADR 0013: Settings in the web UI, and encrypted write-only secrets

- Status: accepted
- Date: 2026-09-27

## Context

The owner asked to manage configuration, including API keys for Zerodha
and the AI providers, from the web UI instead of editing TOML files.
INV-15 requires secrets to stay on the server, and INV-14 requires live
trading to need deliberate server-side configuration.

## Decisions

### Settings: files are defaults, the UI overrides, every run reads them

- Editable sections: `paper`, `risk`, `ai`, `validation` and `review`
  (ADR 0014 adds `kite` and `notifications`)
  (`qd-server/src/runtime.rs`).
- Saving a section validates it through the same typed constructors the
  files use (`RiskConfig::new`, `PaperConfig::validate`, and the others).
  An invalid value is refused and nothing is stored.
- Every save is a new version in `settings_versions`, which is append-only
  (INV-16). Each save is audited with the changed fields' old and new values.
  "Reset to file default" stores an empty version.
- Paper trading, AI, validation, review and research backtests are built
  from the **effective settings on every call**, so a change applies from the
  next run with no restart. The daily schedule re-reads its time every minute.
- **Fail closed:** a saved value that no longer validates, for example after
  an upgrade changed a type, makes every run that needs settings refuse, and
  blocks the server from starting. The UI shows the problem next to the section.
- **Not editable in the UI, on purpose:** `environment`,
  `live_trading_enabled`, the bind address, the account and the database URL.
  No web session can turn on real money (INV-14). The page shows them
  read-only with this reason.
- The paper section has an `enabled` switch. Off means no paper runs, and
  startup reconciliation keeps entries halted (INV-07).
- Every write requires the owner and a password step-up within five minutes,
  plus the CSRF header.

### Secrets: encrypted at rest, write-only through the API

- **Catalog** (`qd-app/src/secrets.rs`): Kite API key, secret and daily
  access token; OpenAI, Gemini and xAI API keys; crypto API key and secret.
  Only these names can be stored.
- **Encryption:** XChaCha20-Poly1305 with a random 24-byte nonce per value,
  and the secret's name as associated data, so a ciphertext cannot be
  swapped onto another name.
- **Master key:** `QD_MASTER_KEY` (64 hex characters) if set. Otherwise a key
  generated on first start in `data_dir/master.key`: mode 0600, created only
  if absent, never overwritten. The key is never in the database, so a
  database dump alone cannot decrypt anything. With neither, the store is
  disabled and the UI says so.
- **Write-only API:** `GET /api/secrets` returns status only (set, readable,
  when, by whom). `PUT` and `DELETE` require the owner and a step-up. No API
  route reads a value; reading is a separate `SecretReader` port that only
  server-side adapters get. `SecretValue` and `MasterKey` redact their `Debug`
  output. The audit log records the name only.
- **The secrets table is mutable, deliberately:** a cleared or rotated key must
  really be gone. Changes are audited.
- Values must be non-empty, at most 4 KB, and free of control characters,
  which catches pasted newlines.

### Deployment

- The container configuration sets `data_dir = "/var/lib/quantdesk"` on a
  named volume (`qdstate`), so the master key is created automatically; the
  owner edits no file.
- **Back the key up separately** from the database dumps. Without it, stored
  keys must be re-entered.

### Also changed

- The advisory-AI daily budget now counts today's journaled advice instead
  of an in-memory counter, so it holds across restarts and across the
  orchestrators built per run.

## Consequences

- The TOML files still work and stay the defaults. A fresh install runs
  without anyone opening one.
- The provider adapters (Kite, crypto, AI) are still not built, because their
  docs are unreachable. The keys can be entered now and will be used, through
  `SecretReader`, when the adapters exist.
