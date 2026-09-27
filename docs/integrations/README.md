# External integrations

Rule (spec, CLAUDE.md): read the provider's current documentation before
writing an adapter, and record what was read here, with the date.

| Provider | Purpose | Status | Page |
|---|---|---|---|
| Zerodha Kite Connect | Indian market data, orders (live, Phase 9) | **Not implemented**: documentation unreachable from the build environment | [kite.md](kite.md) |
| Crypto venue | Crypto market data and orders | **Not implemented**: venue not chosen (ADR 0004); documentation unreachable | [crypto.md](crypto.md) |
| CSV files | Daily bars for any instrument | Implemented: `qd bars import` | [csv-bars.md](csv-bars.md) |
| AI providers (OpenAI, Gemini, xAI) | Advisory analysis (Phase 8) | Not started | — |

## How to unblock the network

The build environment's egress policy denied these hosts on 2026-09-27:
`kite.trade`, `api.binance.com`, `developers.binance.com`, `www.nseindia.com`
(and earlier `zerodha.com`). To let a build session read provider docs, add
those hosts to the allowed domains in the cloud environment's settings
(Network access), or give the environment broader access.
