# Zerodha Kite Connect v3

- Status: **implemented** in `backend/crates/qd-broker-kite` (ADR 0014).
- Read on 2026-09-27 from https://kite.trade/docs/connect/v3/: introduction,
  user and login, market quotes and instruments, historical candles, orders,
  GTT, portfolio, postbacks, exceptions and the changelog. Also read: the
  Zerodha developer-forum announcement on SEBI's retail-algo rules
  (https://kite.trade/forum/discussion/15912), in force since 1 April 2026.

## What the adapter relies on

**Base and headers**
- Base URL: `https://api.kite.trade`, header `X-Kite-Version: 3`.
- Requests are form-encoded; responses are JSON
  `{"status":"success","data":...}`.
- Errors come back as `{"status":"error","message":...,"error_type":...}`.

**Login and session**
- Send the owner to
  `https://kite.zerodha.com/connect/login?v=3&api_key=KEY&redirect_params=<urlencoded>`.
- A successful login redirects to the redirect URL registered in the
  developer console, with `request_token` (valid for a few minutes) and our
  `redirect_params`.
- Exchange it: `POST /session/token` with `api_key`, `request_token` and
  `checksum = sha256(api_key + request_token + api_secret)` (hex). The
  response has `user_id` and `access_token`.
- Sign every request with the header
  `Authorization: token api_key:access_token`.
- The access token expires at 06:00 IST the next day (a regulatory rule), or
  earlier on a master logout. A 403 with `TokenException` means log in again.

**Instruments and candles**
- `GET /instruments[/:exchange]` returns a gzipped CSV with columns
  `instrument_token, exchange_token, tradingsymbol, name, last_price, expiry,
  strike, tick_size, lot_size, instrument_type, segment, exchange`. It is
  generated daily.
- Key instruments by exchange plus tradingsymbol: tokens can be reused after
  expiry.
- `GET /instruments/historical/:token/day?from=YYYY-MM-DD HH:MM:SS&to=...`
  returns `data.candles` rows `[timestamp(+0530), open, high, low, close, volume]`.
  Add `continuous=1` for expired NFO and MCX futures.

**Orders**
- `POST /orders/:variety` with variety `regular` or `amo` (after-market).
- Order fields: `tradingsymbol`, `exchange`, `transaction_type` (BUY|SELL),
  `order_type` (MARKET|LIMIT|SL|SL-M), `quantity`, `product` (CNC|NRML|MIS|MTF),
  `price`, `trigger_price`, `validity` (DAY|IOC|TTL), `market_protection`, `tag`.
- `tag` is alphanumeric, at most 20 characters. The response carries
  `data.order_id`. A placed order is not yet an executed order.
- `DELETE /orders/:variety/:order_id` cancels.
- `GET /orders` lists the day's orders, including `status`, `filled_quantity`,
  `pending_quantity`, `average_price`, `tag` and `meta`.
- `GET /orders/:id` returns the order's history.
- Statuses include `OPEN`, `COMPLETE`, `CANCELLED`, `REJECTED` and
  `TRIGGER PENDING`, plus transient ones such as `PUT ORDER REQ RECEIVED`,
  `AMO REQ RECEIVED` and `OPEN PENDING`.

**Market protection**
- Required since 1 April 2026: MARKET and SL-M orders with protection `0`
  are rejected.
- Values are `1–100` (percent) or `-1` (automatic). The adapter sends the
  configured value, `-1` by default.

**GTT**
- `POST /gtt/triggers` with `type` (`single` | `two-leg`), `condition` (JSON:
  `exchange`, `tradingsymbol`, `trigger_values[]`, `last_price`) and `orders`
  (JSON array of LIMIT orders). The response carries `data.trigger_id`.
- `two-leg` is an OCO: the leg whose trigger is reached is placed, and the
  other lies dormant.
- `GET /gtt/triggers/:id` returns `status` (active | triggered | ...) and
  each order's `result` once placed.
- `DELETE /gtt/triggers/:id` removes a trigger.

**Portfolio**
- `GET /portfolio/holdings` gives delivery holdings: `quantity`,
  `t1_quantity`, `tradingsymbol`, `exchange`.
- `GET /portfolio/positions` gives `net[]` with a signed `quantity` per
  instrument and product.

**Rate limits**
- Quote: 1 request per second. Historical candles: 3 per second. Orders: 10
  per second, 400 per minute and 5,000 per day.
- At most 25 modifications per order.

## Rules that shape the design (SEBI, from 1 April 2026)

- **Static IP:** order endpoints accept requests only from the IP registered
  under "IP Whitelist" on developers.kite.trade. Market data, order book and
  positions work from any IP. The owner must register the server's static IP.
- **Order rate:** at most 10 orders per second. The adapter places orders one
  at a time; a daily-bar system needs a handful.
- **Market protection:** must be non-zero on MARKET and SL-M orders (see above).
- **MCX:** no IOC orders in the algo segment. The adapter uses DAY validity only.

## Mapping from QuantDesk

| QuantDesk | Kite |
|---|---|
| OpenLong / CloseShort | `BUY` |
| CloseLong / OpenShort | `SELL` |
| Delivery / Margin product | `CNC` / `NRML` |
| Market | `MARKET` + `market_protection` |
| Limit | `LIMIT` + `price` |
| StopLimit (entries) | `SL` + `trigger_price` + `price` |
| StopMarket (exits) | `SL-M` + `trigger_price` + `market_protection` |
| Day validity | `DAY`; variety `amo` outside market hours |
| Protective stop + target (one OCO group) | one `two-leg` GTT with LIMIT legs; the stop leg's limit is the stop moved by `stop_limit_buffer` against the exit, rounded to a tick |
| Intent id (idempotency) | `tag` = the last 20 hex characters of the intent's UUID (its random part) |
| Broker positions (reconciliation) | holdings (`quantity + t1_quantity`), plus today's CNC trades (`day_buy_quantity - day_sell_quantity`), plus the net `quantity` of other products; matched to instruments by exchange and tradingsymbol |

## How the adapter behaves

- **Instruments.** An instrument is traded at Kite only if its spec has a
  broker reference with `broker = "kite"` and `symbol` set to the Kite
  trading symbol. `token` is optional; without it, the token comes from the
  instruments list.
- **Bars.** A day's candle is imported once it is complete: after 16:00 IST
  for NSE, BSE and NFO, and after 23:55 IST for MCX. Only dates after the
  last stored bar are written.
- **Entries.** An entry goes out as `amo` outside market hours and `regular`
  inside them (setting `variety`).
- **Protection.** Fills are checked every few minutes during market hours.
  When an entry fills, its stop and target are placed at once as one
  two-leg GTT.
- **Fills.** A fill is read from the day's order book (`filled_quantity`,
  `average_price`). A GTT leg's fill is read from the order the leg placed.
- **Missing records.** The order book covers only the current day. An
  order the adapter cannot find leaves the book unreconciled, and
  reconciliation then halts new entries (INV-06).
- **Unknown outcomes.** An order whose outcome is unknown after a crash is
  looked up by its tag. If it is not found, entries are halted until the
  owner checks Kite.
- **Failed or deleted GTT leg.** When a GTT leg fails (for example it is
  rejected at the circuit limit) or is deleted outside QuantDesk, the
  position is marked unprotected and a critical alert is raised.

Known limits:
- **Stop after a gap.** A stop leg that triggers after a gap beyond its
  limit stays an open LIMIT order at Zerodha. QuantDesk keeps it working
  and does not chase it. Check the Kite order book if a position stays open
  below its stop.
- **Fill price.** A partly filled order reports Zerodha's average price for
  every fill.
- **Real Zerodha untested.** The adapter was tested only against a local
  fake of the documented API. Before real money, place one tiny order by
  hand, then confirm that the journal, the book and Kite agree.

## Owner actions (not automatable)

- Kite Connect plan: "Connect" costs INR 500 per month and includes
  historical data; "Personal" is free (zerodha.com/charges, 2026-09-27).
- Register the redirect URL `https://<your domain>/api/kite/callback` in the
  developer console.
- Register the server's static IP (IP Whitelist) before any live order.
- Set up DDPI (or authorise CDSL TPIN) so delivery sells from holdings are not
  blocked.
- Enable TOTP on the Zerodha account (required for Kite Connect).
- Enter the API key and secret in Settings. Log in daily with "Login with
  Zerodha": tokens expire at 06:00 IST.

## Market quotes (the AI agent, ADR 0016)

- Source: https://kite.trade/docs/connect/v3/market-quotes/ (read 2026-09-27).
- `GET /quote` (full, up to 500 instruments), `GET /quote/ohlc` (OHLC and
  last price, up to 1000), and `GET /quote/ltp` (last price, up to 1000).
  Instruments are repeated `i=EXCHANGE:TRADINGSYMBOL` parameters.
- Instruments missing from `data` have no quote (or expired); check each key.
- The agent uses `/quote/ohlc` for at most 20 symbols per call. In Kite's
  OHLC, `close` is the previous session's close. Quotes inform the agent;
  plans are judged on completed daily bars.
