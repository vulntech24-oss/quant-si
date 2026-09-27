# ADR 0004: Owner decisions (§4) and the defaults in use

- Status: accepted (defaults); items marked OPEN still need the owner
- Date: 2026-09-27

## Context

§4 lists owner decisions. Where the owner has not answered, the spec says to
proceed with the recommended default and record it. None were answered when
coding started.

## Decision

| # | Item | Default in use | Needed by |
|---|---|---|---|
| 1 | Crypto venue | Crypto is data, analysis, backtest and paper only, through a separate market-data adapter. Live crypto execution deferred. **OPEN: which public venue.** | Phase 6 |
| 2 | Gold, silver, crude | Instrument model supports ETFs and futures (done in `qd-domain`). Universe: gold and silver via NSE ETFs; crude via MCX front-month futures, configurable roll, paper-only until MCX broker-side protection is verified. | Phase 2 |
| 3 | Short selling | Long-only for cash equities and ETFs. Shorts only where `can_short_overnight` is true and only for versions validated on shorts. `InstrumentSpec::permits_overnight` implements the instrument half. | Phase 2 |
| 4 | Stock universe | **OPEN.** Proposal: constituents of a liquid index (for example NIFTY 100) kept as a dated file; reports disclose survivorship risk. | Phase 2 |
| 5 | Kite Connect plan | **OPEN.** Assume no market data until the paid plan is confirmed; no Kite code before then. | Phase 6 |
| 6 | Hosting / static IP | **OPEN.** Needed only for live orders. | Phase 9 |
| 7 | Delivery exits (DDPI) | Live equity entries are blocked until automated delivery exits are confirmed. | Phase 9 |
| 8 | AI providers | **OPEN.** Default AI policy is no influence, shadow logging only. No provider enabled. | Phase 8 |
| 9 | Risk parameters | §4 defaults, as configuration: 0.5% per trade; 5% total open risk; 2% per bucket and per strategy version; 2% daily and 4% weekly loss; hard halt at 10% drawdown; cool-off after 3 losses; stage multipliers Paper 0 / SmallCapital 0.25 / Full 1.0 (paper accounts size as Full); 30 out-of-sample setups; EV ≥ +0.10R. | Phase 2 |
| 10 | Frontend approach | **OPEN; depends on §10.** Proposal: rebuild the Stitch screens as a typed SPA (Vite + TypeScript, Tailwind build instead of the CDN) that talks only to the QuantDesk API, keeping the visual design and fixing the known export issues. | Phase 5 |
| 11 | FX source | **OPEN.** Proposal: the crypto venue's USDT/INR or a published USD/INR reference rate, stored with timestamps; entries on non-INR instruments fail closed without a fresh rate. | Phase 6 |

## Consequences

Paper mode stays the default everywhere. Nothing in the current code places
or can place an order.
