# Mean Reversion — Bollinger

> Trades the snap-back when mid pierces a Bollinger band. Maintains a rolling SMA and sample stddev, and places a single limit order at the SMA when the price crosses either band.

## What it does

Tracks a rolling Simple Moving Average (SMA) and its sample standard deviation over
`--window` mid samples. Builds Bollinger bands at `SMA ± k × stddev`. When mid pierces
a band, places a single limit order at the SMA (mean-reversion target). Won't stack:
skips when an open order already exists in the signal direction.

## How it works

Mean Reversion — Bollinger Agent

Keeps a rolling buffer of the last `--window` mid observations. On every poll it
recomputes:

- `sma`     = mean of the last N mids
- `stddev`  = sample stddev of the last N mids
- `upper`   = sma + k × stddev
- `lower`   = sma − k × stddev

Once at least `--warmup-samples` (default = window) samples have accumulated so
that stddev is well-defined, the strategy fires:

- price ≥ upper band  →  OFFER at price = SMA  (price too high, expect to
  revert down)
- price ≤ lower band  →  BID   at price = SMA  (price too low, expect to
  revert up)

At most one open reversion order per direction at any time (to avoid stacking
risk). When that order fills or is cancelled, the agent becomes eligible to
signal again.

## Usage

```bash
agent-mean-reversion-bollinger run --market CC-USDC --quantity 1 --window 20 --k 2.0
agent-mean-reversion-bollinger run --market CC-USDC --quantity 0.5 --poll-secs 10 --warmup-samples 30
agent-mean-reversion-bollinger status
```

## Metadata

| Field | Value |
|---|---|
| Category | `trading` |
| Agent slug | `mean-reversion` |
| Template | `Bollinger` (rust) |
| Status | `active` |
| Tags | `mean-reversion`, `bollinger`, `bands` |
| Required balance | $100 min |
| Supported assets | CC, USDC |
| Supported projects | Silvana |

## Package

- Crate: `agent-mean-reversion-bollinger`
- Binary: `agent-mean-reversion-bollinger`

## Resources

- Live demo: <https://silvana.one/agents/demo/mean-reversion-bollinger/>
- Contact Us: <https://silvana.one/contact>
