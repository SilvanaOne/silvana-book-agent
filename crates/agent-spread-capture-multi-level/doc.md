# Spread Capture — Multi-Level

> Multi-level two-sided market making — posts a ladder of bids and offers around the mid, spaced by a step, with the same inventory clamp as the single-level variant.

## What it does

Instead of one bid and one offer per cycle, this variant posts a stack of `--levels` quotes on
each side. Level `i` is offset from the mid by the inner half-spread plus `(i − 1) × step_bps`,
so the ladder fans out symmetrically:

- `bid_i   = mid × (1 − inner_spread_bps/20000 − step_bps/10000 × (i − 1))`
- `offer_i = mid × (1 + inner_spread_bps/20000 + step_bps/10000 × (i − 1))`

Each level carries `--quantity-per-level` base units. Every `refresh_secs` the agent cancels its
own live orders on the market and re-quotes the whole ladder. The inventory clamp is identical
to the single-level variant: when net inventory rises above `+max_inventory`, the bid side is
skipped entirely; when it drops below `−max_inventory`, the offer side is skipped entirely.

## How it works

Spread Capture Agent — multi-level two-sided market maker

Every refresh cycle:

1. Cancel any existing own orders on the market.
2. Sample the mid.
3. Build a ladder of `--levels` bids and `--levels` offers around the mid.
4. Apply the inventory clamp (skip the whole bid side or the whole offer side).
5. Submit the surviving quotes; each has `--quantity-per-level` base units.

Inventory accounting is process-local — restart resets it to zero.

## Usage

```bash
agent-spread-capture-multi-level run \
    --market CC-USDC \
    --levels 3 \
    --inner-spread-bps 25 \
    --step-bps 25 \
    --quantity-per-level 0.5 \
    --max-inventory 10

agent-spread-capture-multi-level run \
    --market CC-USDC \
    --levels 5 \
    --inner-spread-bps 15 \
    --step-bps 20 \
    --quantity-per-level 0.25 \
    --max-inventory 5 \
    --refresh-secs 15
```

## Metadata

| Field | Value |
|---|---|
| Category | `trading` |
| Agent slug | `spread-capture` |
| Template | `Multi-Level` (rust) |
| Status | `active` |
| Tags | `marketmaker`, `spread`, `multi-level` |
| Required balance | $200 min |
| Supported assets | CC, USDC |
| Supported projects | Silvana |

## Package

- Crate: `agent-spread-capture-multi-level`
- Binary: `agent-spread-capture-multi-level`

## Resources

- Live demo: <https://silvana.one/agents/demo/spread-capture-multi-level/>
- Contact Us: <https://silvana.one/contact>
