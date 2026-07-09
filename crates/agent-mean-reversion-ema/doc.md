# Mean Reversion — EMA

> Trades the snap-back when mid diverges from a rolling EMA by more than a configurable threshold. Places a single limit order at the EMA target and refuses to stack duplicates.

## What it does

Tracks an EMA over `--ema-window` poll samples. When mid diverges by more than
`--deviation-pct` from EMA, places one limit order at the EMA target. Won't stack: skips
when an open order already exists in the signal direction.

## How it works

Mean Reversion Agent

Tracks the exponential moving average (EMA) of a market's mid price over a
rolling window of polls. When price moves more than `--deviation-pct` away
from the EMA, it places a single limit order betting on the snap-back:

- price > EMA × (1 + deviation/100)  →  SELL  at price = EMA  (price too
  high, expect to fall back)
- price < EMA × (1 − deviation/100)  →  BUY   at price = EMA  (price too
  low, expect to rise)

At most one open mean-reversion order per direction at any time (to avoid
stacking risk). When that order fills or is cancelled, the agent becomes
eligible to signal again.

## Usage

```bash
agent-mean-reversion-ema run --market CC-USDC --quantity 1 --ema-window 20 --deviation-pct 1.5
agent-mean-reversion-ema run --market CC-USDC --quantity 0.5 --poll-secs 10 --warmup-samples 30
agent-mean-reversion-ema status
```

## Metadata

| Field | Value |
|---|---|
| Category | `trading` |
| Agent slug | `mean-reversion` |
| Template | `EMA` (rust) |
| Status | `active` |
| Tags | `ema`, `reversion` |
| Required balance | $100 min |
| Supported assets | CC, USDC |
| Supported projects | Silvana |

## Package

- Crate: `agent-mean-reversion-ema`
- Binary: `agent-mean-reversion-ema`

## Resources

- Live demo: <https://silvana.one/agents/demo/mean-reversion-ema/>
- Contact Us: <https://silvana.one/contact>
