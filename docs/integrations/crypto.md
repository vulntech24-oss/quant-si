# Crypto venue

- Status: **not implemented**.
- The venue is an open owner decision (ADR 0004). Binance's documentation was
  checked on 2026-09-27 and could not be fetched (egress policy denied
  `api.binance.com` and `developers.binance.com`).
- When the owner picks a venue: read its current REST docs for daily candles,
  symbol filters (tick size, step size, minimum notional) and fees, record
  them here, then write the adapter. Credentials go in `.env` only.
