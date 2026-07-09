# Liquidity Seeking — Slippage-Bounded

> Adaptive execution — child order sizes are chosen to keep VWAP-vs-mid slippage under a configured budget while walking the available depth.

## What it does

Execute `--total` while keeping VWAP-vs-mid slippage of every child order under `--max-slippage-bps`. Each cycle walks the relevant side of the depth, sizes the child to the maximum quantity that respects the slippage budget (capped by `--max-chunk`), places it, waits for it to clear, repeats.

## How it works

Liquidity-Seeking Execution Agent

Like `agent-iceberg-execution`, but the visible chunk size is **adaptive**:
each cycle the agent polls `GetOrderbookDepth` and sizes the next child
order to the quantity that the top-of-book on the relevant side can absorb
without slipping past `--max-slippage-bps` from the live mid. When the book
is shallow, child orders shrink; when it's deep, they grow up to
`--max-chunk`.

For each cycle:
1. Get depth + mid.
2. Walk the offer (buys) or bid (sells) side accumulating qty until VWAP
   deviates from mid by more than `--max-slippage-bps`. That's the max
   safe size for this cycle.
3. Place a limit order of `min(remaining, max_safe, --max-chunk)` at the
   last acceptable price level.
4. Wait for the order to clear, then loop until `--total` is filled.

## Usage

```bash
agent-liquidity-seeking-slippage-bounded run --market CC-USDC --side buy --total 100 \
    --max-slippage-bps 25 --max-chunk 5 --depth 20
agent-liquidity-seeking-slippage-bounded run --market CC-USDC --side sell --total 50 \
    --max-slippage-bps 50 --max-chunk 10 --max-runtime-secs 1800
```

## Metadata

| Field | Value |
|---|---|
| Category | `trading` |
| Agent slug | `liquidity-seeking` |
| Template | `Slippage-Bounded` (rust) |
| Status | `active` |
| Tags | `adaptive`, `slippage`, `execution` |
| Required balance | $500 min |
| Supported assets | CC, USDC |
| Supported projects | Silvana |

## Package

- Crate: `agent-liquidity-seeking-slippage-bounded`
- Binary: `agent-liquidity-seeking-slippage-bounded`

## Resources

- Live demo: <https://silvana.one/agents/demo/liquidity-seeking-slippage-bounded/>
- Contact Us: <https://silvana.one/contact>
