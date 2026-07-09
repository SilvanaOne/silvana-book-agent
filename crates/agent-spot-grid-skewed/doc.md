# Spot Grid — Skewed

> Passive market-making grid — stacks bids and offers at fixed levels around the mid and lets price walk through them. No RFQ, no fill loops.

## What it does

Passive spot market-maker whose per-level **quantity** is skewed by current base-instrument inventory. The ladder still uses arithmetic spacing (like `Infinite`), but bids shrink and offers grow when the agent is long its target balance — and vice versa when short. This unwinds unwanted inventory faster than a symmetric grid, without abandoning liquidity provision on the opposite side.

## How it works

Every `--refresh-secs`:
1. Fetch the current mid and this party's balance of `--base-instrument`.
2. Compute `skew = clamp((balance − target) / target, −1, +1)`.
3. Derive multipliers:
   - `bid_mult   = max(0, 1 − alpha × skew)`
   - `offer_mult = max(0, 1 + alpha × skew)`
4. Cancel all this party's active orders on the market.
5. For each level i = 1..N, place:
   - BID at `mid × (1 − step_pct × i / 100)` with quantity `base_quantity × bid_mult`
   - OFFER at `mid × (1 + step_pct × i / 100)` with quantity `base_quantity × offer_mult`
   - A side with `mult == 0` is skipped entirely.

`alpha = 0` collapses back to a symmetric grid. `alpha = 1` fully removes one side at maximum imbalance.

## Usage

```bash
agent-spot-grid-skewed run --market CC-USDC \
    --step-pct 0.5 --levels 5 \
    --base-quantity 1.0 --target-balance 100 --alpha 0.6

agent-spot-grid-skewed run --market CC-USDC --base-instrument CC \
    --step-pct 0.25 --levels 8 \
    --base-quantity 0.5 --target-balance 50 --alpha 0.8 --refresh-secs 30

agent-spot-grid-skewed status
```

## Metadata

| Field | Value |
|---|---|
| Category | `trading` |
| Agent slug | `spot-grid` |
| Template | `Skewed` (rust) |
| Status | `active` |
| Tags | `marketmaker`, `grid`, `inventory-aware` |
| Required balance | $500 min |
| Supported assets | CC, USDC |
| Supported projects | Silvana |

## Package

- Crate: `agent-spot-grid-skewed`
- Binary: `agent-spot-grid-skewed`

## Resources

- Live demo: <https://silvana.one/agents/demo/spot-grid-skewed/>
- Contact Us: <https://silvana.one/contact>
