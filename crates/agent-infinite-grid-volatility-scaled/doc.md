# Infinite Grid — Volatility-Scaled

> Grid market-maker with no fixed price range — regenerates a bid/offer ladder around the live mid on every refresh, so the strategy follows the market indefinitely.

## What it does

Sibling of `Mid-Following` where `step_pct` is not a static CLI parameter but is *derived from a rolling realized-volatility estimate*. Grid tightens when the market is calm and widens when it churns — catching fills in either regime without leaving inventory stuck far from mid.

## How it works

Two independent schedules run inside the loop:

1. **Sampler** — every `--sample-secs` seconds:
   - Poll mid and append `ln(mid_t / mid_{t-1})` to a ring buffer of length `--vol-window`.
   - Recompute `sigma = sample stddev of log returns`.
2. **Rebuilder** — every `--refresh-secs` seconds:
   - `raw_step_pct = step_multiplier × sigma × 100`
   - `step_pct = clamp(raw_step_pct, min_step_pct, max_step_pct)`
   - Cancel all this party's active orders on the market.
   - Place `--levels` bids and `--levels` offers around mid using the arithmetic ladder
     `mid × (1 ± step_pct × i / 100)`.

Notes:
- The clamp is important: with only a couple of samples in the window, sigma is unstable — `min_step_pct` guards against a zero-width grid, `max_step_pct` guards against runaway wide grids during flash-vol events.
- `step_multiplier` is a taste dial; 1.5–3× typically produces a grid whose outer levels sit near the recent one-standard-deviation move.

## Usage

```bash
agent-infinite-grid-volatility-scaled run --market CC-USDC \
    --levels 5 --quantity-per-level 0.1

agent-infinite-grid-volatility-scaled run --market CC-USDC \
    --levels 8 --quantity-per-level 0.05 \
    --vol-window 120 --sample-secs 5 --step-multiplier 2.5 \
    --min-step-pct 0.05 --max-step-pct 3.0 --refresh-secs 30

agent-infinite-grid-volatility-scaled status
```

## Metadata

| Field | Value |
|---|---|
| Category | `trading` |
| Agent slug | `infinite-grid` |
| Template | `Volatility-Scaled` (rust) |
| Status | `active` |
| Tags | `grid`, `dynamic`, `volatility`, `adaptive` |
| Required balance | $500 min |
| Supported assets | CC, USDC |
| Supported projects | Silvana |

## Package

- Crate: `agent-infinite-grid-volatility-scaled`
- Binary: `agent-infinite-grid-volatility-scaled`

## Resources

- Live demo: <https://silvana.one/agents/demo/infinite-grid-volatility-scaled/>
- Contact Us: <https://silvana.one/contact>
