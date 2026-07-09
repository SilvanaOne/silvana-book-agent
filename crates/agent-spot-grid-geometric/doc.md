# Spot Grid — Geometric

> Passive market-making grid — stacks bids and offers at fixed levels around the mid and lets price walk through them. No RFQ, no fill loops.

## What it does

Passive spot market-maker whose ladder is built with a **geometric progression** of level offsets around the current mid. Compared to `Static` (fixed `[[markets.bid_levels]]` blocks in `agent.toml`) and `Infinite` (arithmetic dynamic grid), this variant is dense near mid and exponentially wider in the tails — well-suited to markets where you want tight coverage around the current price without wasting inventory on distant rungs.

## How it works

Every `--refresh-secs`:
1. Fetch the current mid.
2. Cancel all this party's active orders on the market.
3. Rebuild the ladder using geometric offsets:
   - `offset_i = (base_step_pct / 100) × ratio^(i-1)` for i = 1..N
   - `bid_price_i  = mid × (1 − offset_i)`
   - `offer_price_i = mid × (1 + offset_i)`
4. Re-quote every level at the configured `--quantity-per-level`.

With `ratio == 1.0` the ladder degenerates to an arithmetic grid. With `ratio > 1` outer rungs get wider multiplicatively.

## Usage

```bash
agent-spot-grid-geometric run --market CC-USDC \
    --base-step-pct 0.25 --ratio 1.5 --levels 6 --quantity-per-level 0.1

agent-spot-grid-geometric run --market CC-USDC \
    --base-step-pct 0.5 --ratio 2.0 --levels 4 --quantity-per-level 0.05 \
    --refresh-secs 30

agent-spot-grid-geometric status
```

## Metadata

| Field | Value |
|---|---|
| Category | `trading` |
| Agent slug | `spot-grid` |
| Template | `Geometric` (rust) |
| Status | `active` |
| Tags | `marketmaker`, `grid`, `geometric` |
| Required balance | $500 min |
| Supported assets | CC, USDC |
| Supported projects | Silvana |

## Package

- Crate: `agent-spot-grid-geometric`
- Binary: `agent-spot-grid-geometric`

## Resources

- Live demo: <https://silvana.one/agents/demo/spot-grid-geometric/>
- Contact Us: <https://silvana.one/contact>
