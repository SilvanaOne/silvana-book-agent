# Trend Following — MACD

> MACD momentum trader — MACD line crosses above signal line → bid entry, below → offer. Won't stack orders in the current signal direction.

## What it does

Classic MACD crossover strategy. Maintains three EMAs on the live mid: a fast EMA (`--fast`, default 12), a slow EMA (`--slow`, default 26), and a signal EMA (`--signal-period`, default 9) taken over the MACD line itself. Emits a BID on bullish crossovers (MACD crosses above signal) and an OFFER on bearish crossovers. Skips a signal when an open own-order already exists in that direction — no stacking.

## How it works

Trend Following Agent — MACD variant.

At each poll the agent:

1. Fetches the live mid from `PricingService.GetPrice`.
2. Updates `EMA_fast` (α = 2 / (`fast` + 1)) and `EMA_slow` (α = 2 / (`slow` + 1)).
3. Computes the **MACD line** = `EMA_fast − EMA_slow`.
4. Updates the **signal line** as an EMA of the MACD line (α = 2 / (`signal_period` + 1)).
5. Watches the sign of `MACD − Signal`. When it flips from negative (or zero) to positive → **BULLISH** signal, place a BID at mid for `--quantity`. When it flips from positive (or zero) to negative → **BEARISH** signal, place an OFFER.

The agent will not stack: before submitting, it calls `GetOrders` and skips the signal if an active/partial own-order in the same direction is already resting.

Warmup: no signals are emitted until at least `--warmup-samples` ticks have been observed (default = `slow + signal_period`), so all three EMAs are stable.

State (last diff sign, last cross, EMAs) is persisted to `trend-following-macd-state.json`.

## Usage

```bash
agent-trend-following-macd run --market CC-USDC --fast 12 --slow 26 --signal-period 9 --quantity 1
agent-trend-following-macd run --market CC-USDC --fast 8 --slow 21 --signal-period 5 --quantity 0.5 --poll-secs 5 --warmup-samples 40
```

## Metadata

| Field | Value |
|---|---|
| Category | `trading` |
| Agent slug | `trend-following` |
| Template | `MACD` (rust) |
| Status | `active` |
| Tags | `trend-following`, `macd`, `momentum` |
| Required balance | $200 min |
| Supported assets | CC, USDC |
| Supported projects | Silvana |

## Package

- Crate: `agent-trend-following-macd`
- Binary: `agent-trend-following-macd`

## Resources

- Live demo: <https://silvana.one/agents/demo/trend-following-macd/>
- Contact Us: <https://silvana.one/contact>
