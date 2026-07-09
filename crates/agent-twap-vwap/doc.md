# TWAP — VWAP

> Volume-weighted TWAP: slice a large parent across time on the same fixed cadence as Linear, but size each slice by a normalized volume curve instead of splitting the total equally. Each slice is priced at mid ± offset, optionally clamped by a worst-acceptable limit.

## What it does

Splits `--total` across `--slices` time steps that are still equally spaced —
one per `duration_secs / slices` seconds — but the *size* of each slice is
proportional to a `--volume-curve` weight vector rather than uniform. This
front-loads or back-loads execution to match a market's expected intraday
volume profile, which typically reduces slippage compared to flat TWAP.

Contrast with the `Linear` sibling: Linear uses `slice_qty = total / slices`
for every slot; VWAP uses `slice_i_qty = total × weight_i / Σ weight`.

## How it works

Per slice `i` (1..=slices) at slot time `startTime + i × interval`:

1. `slice_qty = total × weight_i / Σ weight` — weights normalize themselves,
   so you can pass raw counts (`1,2,3,2,1`) or already-normalized fractions.
2. `raw_price = mid × (1 + price_offset_pct / 100)`.
3. `order_price = round_to_tick(raw_price, tick_size)`.
4. Optional clamp: BID skipped if `price > limit`; OFFER skipped if `price < limit`.
5. Submit signed limit order and continue to the next slot.

Presets accepted by `--volume-curve`:

- `u-shaped` — classic U profile (high open/close, thin mid).
- `linear` — all weights equal (degenerates to plain TWAP).
- `front-loaded` — monotonically decreasing weights.

You can also pass a raw comma list like `1,2,3,2,1`. Its length **must**
match `--slices`, else the run aborts before any order is placed.

## Usage

```bash
# 20 slices over 1h, U-shaped volume curve, buy side
agent-twap-vwap run --market CC-USDC --side buy --total 100 \
    --slices 20 --duration-secs 3600 --volume-curve u-shaped

# Custom weights, sell side with limit-price clamp
agent-twap-vwap run --market CC-USDC --side sell --total 50 \
    --slices 5 --duration-secs 1800 \
    --volume-curve "1,2,3,2,1" \
    --price-offset-pct 0.1 --limit-price 0.95

# Front-loaded execution
agent-twap-vwap run --market CC-USDC --side buy --total 200 \
    --slices 10 --duration-secs 600 --volume-curve front-loaded

agent-twap-vwap status
```

## Metadata

| Field | Value |
|---|---|
| Category | `trading` |
| Agent slug | `twap` |
| Template | `VWAP` (rust) |
| Status | `active` |
| Tags | `twap`, `vwap`, `volume-weighted`, `execution` |
| Required balance | $100 min |
| Supported assets | CC, USDC |
| Supported projects | Silvana |

## Package

- Crate: `agent-twap-vwap`
- Binary: `agent-twap-vwap`

## Resources

- Live demo: <https://silvana.one/agents/demo/twap-vwap/>
- Contact Us: <https://silvana.one/contact>
