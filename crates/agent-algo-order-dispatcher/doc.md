# Algo Order — Dispatcher

> Generic algorithmic order dispatcher with pluggable execution strategies (TWAP, iceberg, participation-rate) selected per parent order.

## What it does

Algo Order Agent — pluggable execution dispatcher

## How it works

Algo Order Agent — pluggable execution dispatcher

Reads a "plan" TOML listing one or more algo executions. Each entry picks
an algorithm and supplies its parameters; the agent runs them sequentially
against the orderbook. Currently supported algorithms (in-process
implementations — not external binaries):

- **twap**: split `total` into `slices` equal pieces at intervals of
  `duration_secs / slices`.
- **iceberg**: place visible chunks, wait for each to clear, repeat until
  `total` is filled.
- **liquidity-seeking**: walk the depth book each cycle, size the chunk to
  the largest qty that respects `max_slippage_bps` (capped by `max_chunk`),
  place, wait, repeat.

Plan TOML:

```toml
[[step]]
algorithm = "twap"
market = "CC-USDC"
side = "buy"
total = "100"
slices = 10
duration_secs = 600
price_offset_pct = 0.0

[[step]]
algorithm = "iceberg"
market = "CC-USDC"
side = "sell"
total = "50"
visible = "1"
price = "1.05"

[[step]]
algorithm = "liquidity-seeking"
market = "BTC-USD"
side = "buy"
total = "5"
max_chunk = "0.1"
max_slippage_bps = 25
depth = 20
```

## Usage

```bash
agent-algo-order-dispatcher --config agent.toml run --plan twap-plan.toml --no-restore
agent-algo-order-dispatcher --config agent.toml --verbose check --plan execution-plan.toml
agent-algo-order-dispatcher --config agent.toml --dry-run run --plan mixed-algos.toml
```

## Metadata

| Field | Value |
|---|---|
| Category | `trading` |
| Agent slug | `algo-order` |
| Template | `Dispatcher` (rust) |
| Status | `active` |
| Tags | `algo`, `execution`, `dispatcher` |
| Required balance | $500 min |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-algo-order-dispatcher`
- Binary: `agent-algo-order-dispatcher`

## Resources

- Live demo: <https://silvana.one/agents/demo/algo-order-dispatcher/>
- Contact Us: <https://silvana.one/contact>
