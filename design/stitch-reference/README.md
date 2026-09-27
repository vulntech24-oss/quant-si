# Stitch export: visual reference only

Google Stitch generated these screens for QuantDesk. The owner provided them as
a **visual reference** (layout, colors, typography, card style). They are not
product requirements, and none of their data is real.

| Folder | Content |
|---|---|
| `calm_quantitative_precision/DESIGN.md` | Design system: tokens, type scale, layout, components |
| `quantdesk_home_overview/` | Home: greeting, market strip, opportunity cards, portfolio snapshot |
| `quantdesk_discover_opportunities/` | Screener table with asset-class filters |
| `quantdesk_asset_analysis_btc_usd/` | Single-asset analysis with a projection chart and factor tabs |
| `quantdesk_strategy_backtests/` | Backtest summary, equity curve, monthly returns |
| `quantdesk_mark/quantdesk_mark.svg` | Logo mark (exported as `code.html`; renamed because it is an SVG) |

Each screen folder has `code.html` (static HTML, Tailwind Play CDN) and
`screen.png`.

## Keep

The calm light palette, Geist + JetBrains Mono typography, white cards on a
dotted canvas, the four-corner navigation shell, and progressive disclosure
for deep analysis.

## Do not carry over (conflicts with the spec)

- Bare `BUY` / `SELL` / `HOLD` chips. Use the §6.3 labels, for example
  "BUY (open long)" (INV-12).
- "Confidence 82% / Exp. Return +12.4%" cards. A proposal shows the plan
  (entry, stop, target, time exit), net RR, costs, outcome probabilities with
  evidence count, EV in R, strategy version and the strongest argument
  against (§6.4).
- No NO TRADE or risk-blocked state is designed. The headline must be post-risk
  (INV-17), and NO TRADE is frequent.
- No paper/live mode indicator or halt state. The top-right corner should show
  mode, halt state and data freshness.
- Position size as "% of a fund benchmark" and "Add to Portfolio Allocator".
  Size comes from the Risk Gate.
- HFT-style content: order-book imbalance, CVD, funding rates, millisecond
  latencies, 15-minute data, scalping models. V1 uses completed daily bars.
- Instruments Kite cannot trade (S&P 500, NASDAQ stocks, NYMEX WTI, XAU/USD
  spot, BTC perpetuals). V1 is NSE stocks, gold/silver ETFs, MCX crude, and
  crypto for paper only.
- "Deploy to Live Sandbox" and "Next Predictive Retrain". Promotion is
  human-gated with evidence (INV-11); AI never retrains or redeploys (INV-04).
- Times in UTC. Display IST (§6.1).

## Known defects in the export

- JetBrains Mono is never loaded and the font stack has no fallback, so all
  numbers render in a serif font. `tabular-nums` is never applied.
- The fixed header is 88px tall on desktop but pages reserve 56px; content
  is hidden under it (Discover's "Scanner Active" line, the BTC header card).
- The BTC chart label "E[R]: $75,780" is clipped at the SVG edge; the
  backtest benchmark line draws outside the plot area.
- Tailwind Play CDN is not for production; the radius scale in the HTML
  config differs from `DESIGN.md`; `DESIGN.md`'s YAML tokens (primary
  `#000000`, green `#006c4a`) contradict its prose (`#0F172A`, `#059669`).
- The mock numbers contradict each other (for example the backtest shows
  2022 as +1.2% although its months compound to −1.5%, and the BTC page
  claims RR 3.8:1 where its own stop and target give 1.57:1).
- The logo is hotlinked from Google with `alt="Profile"`; nav links are `#`;
  "Execute Simulation" is a fake spinner; scrollbars are hidden; tabs and
  toggles lack ARIA.
