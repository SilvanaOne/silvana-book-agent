# Strategy Genesis — Keyword

> Chat-to-Strategy compiler: a short natural-language execution spec (`buy 100 CC-USDC over 1 hour with TWAP`) is translated into a valid TOML plan for agent-algo-order. Deterministic keyword parser here; swap in an LLM behind the same interface in prod.

## What it does

Strategy Genesis Agent — Chat-to-Strategy compiler

## How it works

Strategy Genesis Agent — Chat-to-Strategy compiler

Reads a natural-language execution spec and emits an equivalent TOML
plan for `agent-algo-order`. Deterministic keyword-based parser here
(drop-in a real LLM behind the same interface in prod).

Supported input patterns:
  "buy 100 CC-USDC over 1 hour using TWAP"
  "sell 50 BTC-USD with iceberg, visible 5, price 60000"
  "buy 200 CC-USDC minimize slippage, max slippage 25 bps"

Output is a valid TOML file consumable by `agent-algo-order run --plan`.

## Usage

```bash
agent-strategy-genesis-keyword emit --spec "buy 100 CC-USDC over 1 hour using TWAP" --output plan.toml
agent-strategy-genesis-keyword batch --specs-file strategies.txt --output generated-plan.toml
agent-strategy-genesis-keyword emit --spec "sell 50 BTC-USD with iceberg, visible 5, price 60000"
```

## Metadata

| Field | Value |
|---|---|
| Category | `intelligence` |
| Agent slug | `strategy-genesis` |
| Template | `Keyword` (rust) |
| Status | `active` |
| Tags | `chat-to-strategy`, `nlp`, `codegen` |
| Required balance | None — offline |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-strategy-genesis-keyword`
- Binary: `agent-strategy-genesis-keyword`

## Resources

- Live demo: <https://silvana.one/agents/demo/strategy-genesis-keyword/>
- Contact Us: <https://silvana.one/contact>
