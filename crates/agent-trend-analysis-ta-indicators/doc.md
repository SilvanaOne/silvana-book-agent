# Trend Analysis — TA Indicators

> Publishes SMA, EMA, Bollinger bands, RSI, and MACD per market on a fixed cadence — the technical-analysis feed for downstream strategies.

## What it does

Per market polls mid, keeps a rolling buffer, and publishes SMA / EMA / Bollinger / RSI / MACD on every tick to stdout / JSONL file / webhook. Read-only.

## How it works

Trend Analysis Agent — technical-analysis indicator publisher

For each market, polls the mid price at a fixed interval, maintains a
rolling buffer of the last `--window` samples, and computes a small set of
technical indicators each tick:

- SMA (simple moving average)
- EMA (exponential moving average), α = 2 / (window + 1)
- Bollinger Bands (upper, lower) at ± k × standard deviation
- RSI (Wilder, 14-period default within the window)
- MACD (12-EMA − 26-EMA) using sub-windows when the buffer permits

Publishes one JSON record per market per tick to stdout / JSONL file /
HTTP webhook. Read-only — no orders, no ledger writes.

## Usage

```bash
agent-trend-analysis-ta-indicators run --markets CC-USDC --window 30 --stdout
agent-trend-analysis-ta-indicators run --markets CC-USDC,BTC-USD --window 60 --rsi-period 14 \
                        --bollinger-k 2.0 --webhook https://hooks.example.com/ta
```

## Metadata

| Field | Value |
|---|---|
| Category | `data_analytics` |
| Agent slug | `trend-analysis` |
| Template | `TA Indicators` (rust) |
| Status | `active` |
| Tags | `ta`, `indicators`, `publisher` |
| Required balance | None — read-only |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-trend-analysis-ta-indicators`
- Binary: `agent-trend-analysis-ta-indicators`

## Resources

- Live demo: <https://silvana.one/agents/demo/trend-analysis-ta-indicators/>
- Contact Us: <https://silvana.one/contact>
