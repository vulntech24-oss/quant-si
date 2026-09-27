# Daily bars from CSV (`qd bars import`)

- Status: implemented (Phase 4). This is the market-data path until a
  provider adapter exists.
- Format: header `date,open,high,low,close,volume`, one completed daily bar
  per row, ISO dates, decimal prices as plain numbers.
- Every row is validated (high ≥ open/close ≥ low, positive prices, no
  duplicate or unordered dates) before anything is written.
- Point-in-time (INV-09): each import records `ingested_at`; a corrected bar
  is a new row, and earlier snapshots keep seeing the old value.
- Daily routine for paper trading: import the day's completed bars after the
  close, then `qd paper run --config <file>` (or let the server's
  `daily_run_utc` schedule do it).

```sh
qd bars import --instrument <INSTRUMENT_ID> bars.csv
```
