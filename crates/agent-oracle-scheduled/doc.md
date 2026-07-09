# Oracle — Scheduled

> Scheduled price publisher — polls GetPrice for every configured market on a fixed cadence and emits one record per market per poll to stdout, JSONL file, or webhook.

## What it does

Polls `GetPrice` for every market on a schedule and emits one record per market per poll to sinks. Optional `--source` forces a specific upstream (binance_spot / bybit / coingecko).

## How it works

Oracle Agent — schedule-based price publisher

Polls `GetPrice` for every market in `--markets` on a fixed interval and
publishes timestamped quotes to one or more sinks (stdout / append JSONL /
HTTP webhook). Lets a downstream consumer treat Silvana's aggregated price
feed as a structured oracle stream without needing direct gRPC access.

## Usage

```bash
agent-oracle-scheduled run --markets CC-USDC,BTC-USD --poll-secs 30 --stdout
agent-oracle-scheduled run --markets CC-USDC --source coingecko --webhook https://hooks.example.com/oracle
```

## Metadata

| Field | Value |
|---|---|
| Category | `data_analytics` |
| Agent slug | `oracle` |
| Template | `Scheduled` (rust) |
| Status | `active` |
| Tags | `price-feed`, `scheduled` |
| Required balance | None — read-only |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-oracle-scheduled`
- Binary: `agent-oracle-scheduled`

## Resources

- Live demo: <https://silvana.one/agents/demo/oracle-scheduled/>
- Contact Us: <https://silvana.one/contact>
