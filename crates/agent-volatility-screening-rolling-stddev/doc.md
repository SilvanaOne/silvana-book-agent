# Volatility Screening — Rolling Std-Dev

> Maintains a rolling window of log returns and publishes the annualized realized volatility per market. Read-only — feeds downstream risk and quoting agents.

## What it does

Maintains a rolling window of log returns over the last `--window` ticks and publishes the sample std-dev annualized to `realized_vol_annualized = std × sqrt(periods_per_year)`.

## How it works

Volatility Screening Agent — rolling realized-vol publisher

For each market polls the mid price every `--poll-secs` and maintains a
rolling window of the last `--window` samples. After each tick it computes:

- log return at this sample (ln(p_t / p_{t-1}))
- sample std-dev of log returns over the window
- **realized volatility** (annualized) = std × sqrt(periods_per_year)
  where periods_per_year = (365·24·3600) / poll_secs assuming continuous
  sampling; this is correct for a tick-level vol estimate.

Publishes one record per market per tick to stdout / JSONL file / webhook.

## Usage

```bash
agent-volatility-screening-rolling-stddev run --markets CC-USDC --window 100 --stdout
agent-volatility-screening-rolling-stddev run --markets CC-USDC,BTC-USD --window 200 --poll-secs 10 \
                              --log-file vol.jsonl
```

## Metadata

| Field | Value |
|---|---|
| Category | `risk` |
| Agent slug | `volatility-screening` |
| Template | `Rolling Std-Dev` (rust) |
| Status | `active` |
| Tags | `realized-vol`, `rolling-window` |
| Required balance | None — read-only |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-volatility-screening-rolling-stddev`
- Binary: `agent-volatility-screening-rolling-stddev`

## Resources

- Live demo: <https://silvana.one/agents/demo/volatility-screening-rolling-stddev/>
- Contact Us: <https://silvana.one/contact>
