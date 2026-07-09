# Fair Value — Multi-Source

> Multi-source reference price — polls every configured upstream (binance, bybit, coingecko, …) for each market and aggregates with median, mean, or trimmed-mean.

## What it does

For each market polls every `--sources` entry separately and aggregates with `--method` (`median`, `mean`, or `trimmed-mean`). Emits one fair-value record per market per poll.

## How it works

Fair Value Screening Agent

For each market, polls `GetPrice` against every configured source
(`binance_spot`, `bybit`, `coingecko`, …) and computes an aggregate "fair
value" using one of `median`, `mean`, or `trimmed_mean` (drops the highest
and lowest before averaging). Emits one fair-value record per market per
poll to stdout / append JSONL / HTTP webhook.

## Usage

```bash
agent-fair-value-multi-source run --markets CC-USDC --sources binance_spot,bybit,coingecko --stdout
agent-fair-value-multi-source run --markets CC-USDC --sources binance_spot,bybit --method mean --webhook https://...
```

## Metadata

| Field | Value |
|---|---|
| Category | `risk` |
| Agent slug | `fair-value` |
| Template | `Multi-Source` (rust) |
| Status | `active` |
| Tags | `aggregation`, `reference-price` |
| Required balance | None — read-only |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-fair-value-multi-source`
- Binary: `agent-fair-value-multi-source`

## Resources

- Live demo: <https://silvana.one/agents/demo/fair-value-multi-source/>
- Contact Us: <https://silvana.one/contact>
