# Trend Following — EMA Crossover

> EMA-crossover momentum trader — fast crosses slow → bid entry, fast below slow → offer. Won't stack orders in the current signal direction.

## What it does

Two EMAs (`--fast`, `--slow`). Fast crosses above slow → BID entry; below → OFFER. Won't stack: skips when an open order in the signal direction already exists. `--warmup-samples` lets the EMAs stabilize before any signal is emitted.

## How it works

Trend Following Agent — EMA crossover momentum

Maintains two EMAs of the live mid: `fast_ema` (period `--fast`) and
`slow_ema` (period `--slow`). At each poll:

- Fast crosses **above** slow (was ≤, now >) → **BULLISH** signal: place a
  BID at mid for `--quantity` (entry long).
- Fast crosses **below** slow (was ≥, now <) → **BEARISH** signal: place an
  OFFER at mid for `--quantity` (entry short, or exit long).

Skips re-entry on the same side while a previous order is still open
(prevents stacking). Warm-up: `--warmup-samples` ticks are observed before
any signal is emitted, so the EMAs are stable.

## Usage

```bash
agent-trend-following-ema-crossover run --market CC-USDC --fast 9 --slow 21 --quantity 1
agent-trend-following-ema-crossover run --market CC-USDC --fast 5 --slow 50 --quantity 0.5 --poll-secs 5 --warmup-samples 60
```

## Metadata

| Field | Value |
|---|---|
| Category | `trading` |
| Agent slug | `trend-following` |
| Template | `EMA Crossover` (rust) |
| Status | `active` |
| Tags | `momentum`, `ema`, `crossover` |
| Required balance | $200 min |
| Supported assets | CC, USDC |
| Supported projects | Silvana |

## Package

- Crate: `agent-trend-following-ema-crossover`
- Binary: `agent-trend-following-ema-crossover`

## Resources

- Live demo: <https://silvana.one/agents/demo/trend-following-ema-crossover/>
- Contact Us: <https://silvana.one/contact>
