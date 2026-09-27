# ADR 0014: Zerodha Kite, live trading, hosted AI advisors and Telegram

- Status: accepted
- Date: 2026-09-27

## Context

The owner allowed network access to the providers and asked for the
remaining work to be completed:
- the Zerodha Kite connection (prices, live orders, account sync);
- the AI providers;
- automatic demotion on a breach;
- cost-rate verification;
- notifications.

Every integration below was written from the provider's current
documentation, recorded in `docs/integrations/`. Tests run against local
fake servers only: no real order was placed, and no live order endpoint was
called.

## Decisions

### Costs verified

- The three Zerodha cost schedules were checked rate by rate against
  zerodha.com/charges on 2026-09-27 and marked `verified` with the source.
- The DP charge is ₹13 + GST (₹15.34) per scrip per day on sells.
- STT on ETFs is not modelled; ETFs are not in the universe yet.
- A later rate change needs a new schedule version with a new `checked_on`.

### One OCO order for protection

- `OrderGateway::submit_oco` validates and journals every leg before one
  executor call. If any leg fails validation, no leg is sent.
- `BrokerOrderExecutor::submit_oco` places the legs one after the other by
  default, so simulated venues and backtests are unchanged. The Kite
  executor places one two-leg GTT.
- If the pair is refused, the Position Manager sends the stop alone. A stop
  without a target is safer than no stop.

### `qd-broker-kite`

**Client and login**
- Kite errors are classified by what is known about the outcome:
  - Token, Input, Order, Margin, Holding and User exceptions mean nothing
    was done, so they are rejections;
  - network, data and general exceptions, 5xx responses and timeouts leave
    the outcome unknown (INV-06).
- The login uses a one-time `state` (24 random bytes, valid 10 minutes,
  single use) sent in `redirect_params`.
- The callback needs no session cookie, because the session cookie is
  `SameSite=Strict` and does not come with Zerodha's cross-site redirect.
  The state proves that an owner started the login.
- The access token is stored only if the session's `user_id` is the
  configured client id.

**Orders** (placement refused before any HTTP call unless the build has
`live-orders`, INV-14):
- Entries are regular or AMO orders by exchange hours.
- `market_protection` (default −1, automatic) is set on MARKET and SL-M
  orders, as SEBI requires.
- MCX has no IOC. Good-till-cancelled orders become GTTs.
- The tag is the last 20 hex digits of the intent id.
- The GTT stop leg is a LIMIT at the stop moved by `stop_limit_buffer`
  (default 1%) against the exit, rounded away from the stop to a tick.
- The GTT needs the last price, taken from `/quote/ltp`. If the price is
  already beyond a trigger, the GTT is refused, the position is marked
  unprotected, and a critical alert is raised.

**Fills and positions**
- Fills are read from the day's order book (GTT legs through the order the
  leg placed) and queued per instrument for the session. The same daily
  cycle then applies them (INV-08).
- Positions are:
  - holdings (`quantity + t1_quantity`);
  - plus today's CNC trades;
  - plus the net of other products.

**Restarts**
- The broker id of every acknowledged order is journaled and restored.
- Orders with an unknown outcome are looked up by tag. If one is not found,
  an operational halt stops new entries.

### Live runner (`qd_broker_kite::runner`)

- **Daily cycle.** It processes only the **latest** completed, unprocessed
  date, and never trades on a missed older date: that data is stale for a
  new order. Bars more than 4 days old are refused.
- **Intraday fill check.** During market hours it applies fills, so a
  filled entry gets its GTT at once instead of after the close.
- **Automatic demotion (INV-11).** Before and after each run, every version
  at SmallCapital or Full that an active hard halt covers (global, the live
  account, or the version) moves to Paper with `AutoDemote`.
- **Reconciliation (INV-06, INV-07).**
  - In live mode, the session records an operational halt that needs a
    human re-arm when the book disagrees with Zerodha, or when Zerodha's
    positions cannot be read. The halt comes before any decision is made.
  - Startup reconciles the live book with Zerodha too. Without a Kite
    session, entries stay halted until the owner logs in and re-arms.
- **Protective order lost.** A protective order that ends without a fill
  makes the position unprotected, which raises a critical alert.
- **Configuration.** The live book is set only in the server file
  (`[live]`: account, capital, start date, run time), never in the web UI
  (ADR 0013).
- **Arming.** "Arm" targets the live account when one is configured.
- **Manual runs.** A live run from the UI needs the owner and a step-up.
  Every INV-14 condition is checked again in the gateway.
- **Shared code.** The paper and live runners share `qd_app::runs` (state
  loading, stage slots, instruments, book JSON).

### Hosted advisors (`qd-ai-providers`)

- **Providers.**
  - OpenAI and xAI use the Responses API with a strict JSON schema.
  - Gemini uses `generateContent` with `responseJsonSchema`
    (`responseSchema` is deprecated).
- **Validation and naming.** Answers go through the same `finalize`
  validation as every advisor. The advisor name is `provider:model:v1`, so
  a new model is scored as a new advisor.
- **Keys and INV-04.** The server passes each key as a string. The crate
  depends only on `qd-ai`, `qd-app` and HTTP libraries, and a test checks
  it.
- **Settings.** Default models are those in the providers' docs on the read
  date, and can be changed in Settings. A provider runs only when it is
  switched on and its key is set.

### Telegram

- `sendMessage` is sent with the stored bot token and the configured chat
  id.
- Sent: raised alerts at or above the chosen severity, daily summaries
  (counts only), and failures of scheduled jobs (once a day each).
- Errors are reported without the URL, because the URL contains the token.

## Assumptions to confirm

- **Kite plan.** "Connect" (₹500/month) is needed for historical candles.
- **Static IP.** A static IP must be registered for order endpoints
  (SEBI, from 1 April 2026).
- **Other account setup.** DDPI or TPIN, and TOTP on the account.
- **Live capital.** `[live] initial_equity` is the capital the risk limits
  size from. It is not read from Zerodha's margins.
- **Session expiry.** Tokens expire at 06:00 IST. Until the owner logs in,
  fill checks fail (one Telegram message a day), and a restart keeps entries
  halted.
- **Crypto venue.** Not built: Binance answered HTTP 451 (legal geo-block)
  from the build environment. The venue is the owner's choice.

## Consequences

- Real money is still off by default. It needs:
  - the `live-orders` build;
  - `environment = "production"`;
  - `live_trading_enabled = true`;
  - verified costs (now true);
  - a `[live]` book;
  - an armed live account;
  - a version at SmallCapital or Full, which needs a passed paper review;
  - today's Zerodha login.
- Before real money, place one tiny order by hand and confirm that the
  journal, the book and Kite agree. The adapter has been tested only
  against a fake of the documented API.
