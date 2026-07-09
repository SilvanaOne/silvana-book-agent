# Trend Analysis — Multi-Timeframe

> Per market, maintain three simultaneous SMAs — short / mid / long — and publish a confluence signal describing whether all timeframes point the same way.

## What it does

Read-only technical-analysis publisher focused on multi-timeframe **confluence** rather than a bag of indicators. On every poll the agent updates three rolling SMAs of different lengths per market, computes each SMA's slope versus the previous poll, and emits a compact JSON record with an `alignment` verdict — `aligned_up` when every timeframe is rising, `aligned_down` when every timeframe is falling, `mixed` otherwise. Downstream strategies subscribe to this feed to gate entries only when short-, mid-, and long-term trends agree.

## How it works

For each market on every poll:

- Push the current mid into a rolling buffer capped at `--long` samples.
- Compute:
  - `sma_short` — mean of the last `--short` samples
  - `sma_mid`   — mean of the last `--mid` samples
  - `sma_long`  — mean of the last `--long` samples
- Compute `slope_short`, `slope_mid`, `slope_long` as the delta between the current SMA value and its previous poll's value.
- Derive `alignment`:
  - `aligned_up`   if all three slopes are strictly positive
  - `aligned_down` if all three slopes are strictly negative
  - `mixed`        otherwise
- Emit one JSON record per market per poll (`kind = "trend.multi_timeframe"`).

Warmup: nothing is emitted until the buffer has at least `--long` samples.

## Usage

```bash
agent-trend-analysis-multi-timeframe run --markets CC-USDC --stdout
agent-trend-analysis-multi-timeframe run --markets CC-USDC,BTC-USD \
    --short 5 --mid 20 --long 60 --poll-secs 30 --stdout
agent-trend-analysis-multi-timeframe run --markets CC-USDC \
    --short 10 --mid 30 --long 90 --log-file mtf.jsonl \
    --webhook https://hooks.example.com/mtf
```

CLI flags:

| Flag | Default | Meaning |
|---|---|---|
| `--markets` | required | Comma-separated market ids |
| `--short`   | `5`      | Fast SMA window (samples) |
| `--mid`     | `20`     | Mid SMA window (samples) |
| `--long`    | `60`     | Slow SMA window (samples) |
| `--poll-secs` | `30`   | Poll cadence in seconds |
| `--stdout` / `--log-file` / `--webhook` | at least one required | Sinks |

Constraint: `short < mid < long`, each `>= 2`.

## Metadata

| Field | Value |
|---|---|
| Category | `data_analytics` |
| Agent slug | `trend-analysis` |
| Template | `Multi-Timeframe` (rust) |
| Status | `active` |
| Tags | `trend-analysis`, `multi-timeframe`, `confluence` |
| Required balance | None — read-only |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-trend-analysis-multi-timeframe`
- Binary: `agent-trend-analysis-multi-timeframe`

## Resources

- Live demo: <https://silvana.one/agents/demo/trend-analysis-multi-timeframe/>
- Contact Us: <https://silvana.one/contact>
