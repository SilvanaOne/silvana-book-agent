# TWAP Execution — Linear

> Slice a large parent order into equal time-spaced child orders to reduce market impact. Each slice is priced at mid ± offset, optionally clamped by a worst-acceptable limit.

## What it does

Slice `--total` into `--slices` equal pieces, place one limit order per slice every
`duration_secs / slices` seconds. Each slice's price is mid ± `--price-offset-pct`, clamped
by optional `--limit-price` (worst acceptable).

## How it works

TWAP Execution Agent — slice a large order across time

Splits `--total` quantity into `--slices` equal pieces and places one limit
order per slice spaced `duration / slices` seconds apart. Each slice's
price is the current mid + `--price-offset-pct`, optionally clamped by a
`--limit-price` (worst acceptable price — buys skip if price > limit, sells
skip if price < limit).

## Usage

```bash
agent-twap-linear run --market CC-USDC --side buy --total 100 --slices 20 --duration-secs 3600
agent-twap-linear run --market CC-USDC --side sell --total 50 --slices 10 --duration-secs 1800 \
              --price-offset-pct 0.1 --limit-price 0.95
agent-twap-linear status
```

## Metadata

| Field | Value |
|---|---|
| Category | `trading` |
| Agent slug | `twap` |
| Template | `Linear` (rust) |
| Status | `active` |
| Tags | `execution`, `time-sliced` |
| Required balance | $100 min |
| Supported assets | CC, USDC |
| Supported projects | Silvana |

## Package

- Crate: `agent-twap-linear`
- Binary: `agent-twap-linear`

## Resources

- Live demo: <https://silvana.one/agents/demo/twap-linear/>
- Contact Us: <https://silvana.one/contact>
