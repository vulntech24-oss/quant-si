# Zerodha Kite Connect

- Status: **not implemented**.
- Checked: 2026-09-27. `https://kite.trade/docs/connect/v3/` could not be
  fetched (egress policy denied `kite.trade`), so no adapter was written from
  memory, as the project rules require.

## What an adapter will need (to confirm against the docs)

- Market data: daily historical candles for the stock universe, and the
  instrument dump (tick size, lot size, exchange tokens) to fill
  `InstrumentSpec` broker references.
- Orders (Phase 9, behind the `live-orders` feature): the order-placing call
  must live only in a `BrokerOrderExecutor` handed to the Order Gateway (INV-01).
- Credentials: API key, API secret and the daily access token. They stay on
  the server (INV-15). Set them in your local `.env`; never paste them into a
  conversation. Proposed variable names: `QD_KITE_API_KEY`,
  `QD_KITE_API_SECRET`, `QD_KITE_ACCESS_TOKEN`.
- Owner decisions still open (ADR 0004): Kite plan (historical data add-on),
  static IP for order APIs, DDPI for delivery sells.

## Until then

Daily bars come in through `qd bars import` (see [csv-bars.md](csv-bars.md)),
and trading is paper only.
