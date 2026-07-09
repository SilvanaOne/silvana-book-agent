# Circuit Breaker — Deviation

> Watches mid-price deviation over a rolling window and auto-cancels every order on the market when the configured threshold trips. Reactivates after a cool-down.

## What it does

Tracks a market's mid-price baseline over a rolling window. When deviation exceeds the threshold, cancels all this party's orders on that market and pauses.

## How it works

Circuit Breaker Agent

Tracks a market's mid price. When the price moves more than
`--max-deviation-pct` from a baseline within the `--window-secs` lookback
window, the breaker TRIPS: cancel-all this party's orders on that market and
sleep for `--pause-secs` before re-arming. On re-arm the baseline resets to
the current price.

Use it as a safety net in front of `agent-spot-grid` or any market-making
strategy: a sudden spike or crash pulls your quotes off the book until the
market settles.

## Usage

```bash
agent-circuit-breaker-deviation run --market CC-USDC --max-deviation-pct 5.0 --window-secs 60
agent-circuit-breaker-deviation run --market CC-USDC --max-deviation-pct 2.5 --pause-secs 600 --dry-run
```

## Metadata

| Field | Value |
|---|---|
| Category | `risk` |
| Agent slug | `circuit-breaker` |
| Template | `Deviation` (rust) |
| Status | `active` |
| Tags | `circuit-breaker`, `auto-pause` |
| Required balance | None — read-only |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-circuit-breaker-deviation`
- Binary: `agent-circuit-breaker-deviation`

## Resources

- Live demo: <https://silvana.one/agents/demo/circuit-breaker-deviation/>
- Contact Us: <https://silvana.one/contact>
