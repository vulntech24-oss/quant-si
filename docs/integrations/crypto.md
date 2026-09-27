# Crypto venue

- Status: **not implemented**; it needs an owner decision.
- Checked 2026-09-27 from the build environment:
  - `developers.binance.com` is reachable;
  - `api.binance.com` answers **HTTP 451 (Unavailable For Legal Reasons)**:
    Binance refuses API connections from this server's region. That is a
    legal geo-block, not something the network allow-list can change.
- Before building an adapter, choose a venue that serves your country and
  that QuantDesk's server can reach from where it is deployed. For an India
  deployment: Binance (if reachable from your server), CoinDCX, Delta
  Exchange India or another FIU-registered exchange.
  - Tell me which one, and allow its API and docs hosts.
  - Use API keys without withdrawal rights (Settings → API keys → crypto).
