# Orderbook Streaming — Depth Fanout

> Fans live orderbook depth and external prices for the configured markets to any combination of stdout, append JSONL, and HTTP webhook. Read-only, no party-specific state.

## What it does

Subscribes to internal depth + external price stream for the given markets and forwards every update to any combination of stdout / append JSONL / HTTP webhook. Read-only, no party-specific state.

## How it works

Orderbook Streaming Agent — fan out market data to JSONL sinks

Subscribes to internal Silvana orderbook depth and the external price stream
for a list of markets and forwards every update to any combination of stdout,
append-only JSONL file, and HTTP webhook. Pure data fanout: no orders, no
ledger writes, no party-specific state.

## Usage

```bash
agent-orderbook-streaming-depth-fanout run --markets CC-USDC --stdout
agent-orderbook-streaming-depth-fanout run --markets CC-USDC,BTC-USD --log-file ticks.jsonl --depth 20
agent-orderbook-streaming-depth-fanout run --markets CC-USDC --webhook https://hooks.example.com/feed --include-trades
```

## Metadata

| Field | Value |
|---|---|
| Category | `data_analytics` |
| Agent slug | `orderbook-streaming` |
| Template | `Depth Fanout` (rust) |
| Status | `active` |
| Tags | `market-data`, `fanout` |
| Required balance | None — read-only |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-orderbook-streaming-depth-fanout`
- Binary: `agent-orderbook-streaming-depth-fanout`

## Resources

- Live demo: <https://silvana.one/agents/demo/orderbook-streaming-depth-fanout/>
- Contact Us: <https://silvana.one/contact>
