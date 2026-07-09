# Volatility Screening — Parkinson

> Range-based realized-volatility publisher. Estimates per-period sigma from per-period high/low pairs using the Parkinson (1980) high-low estimator instead of close-to-close log returns. Read-only.

## What it does

For each configured market, samples the current mid every `--poll-secs` seconds and observes the intra-period high/low (using best bid/ask when available as a proxy; otherwise the min/max of mids seen inside the period). At the end of every `--period-secs` window, the bar is frozen as a `(H, L)` pair and added to a rolling window of the last `--window` completed bars.

Once at least two bars have closed, the agent publishes:

- Parkinson per-period variance:
  `sigma² = (1 / (4 · ln 2)) · mean( ln(H/L)² )`
- Per-period sigma: `sqrt(sigma²)`
- Annualized sigma: `sigma × sqrt(N)` where `N = --periods-per-year` (default `525600`, i.e. minute periods)

One JSON record per market per poll is dispatched to any combination of stdout / append JSONL / HTTP webhook. No orders, no ledger writes.

## Why Parkinson

Close-to-close (rolling std-dev of log returns) discards the intra-period trajectory and typically underestimates true volatility. Parkinson uses the observed range and — for a driftless log-normal price process — is roughly 5× more efficient than the classical estimator for the same sample size. In practice this means smoother, more responsive vol signals for the same window length.

## Usage

```bash
agent-volatility-screening-parkinson run --markets CC-USDC --stdout
agent-volatility-screening-parkinson run --markets CC-USDC,BTC-USD \
    --window 60 --period-secs 60 --poll-secs 5 \
    --log-file vol.jsonl
agent-volatility-screening-parkinson run --markets CC-USDC \
    --periods-per-year 525600 \
    --webhook https://hooks.example.com/vol
```

CLI:

- `--markets` — comma-separated list of market IDs.
- `--window` (default `60`) — number of completed high/low bars kept in the rolling window.
- `--period-secs` (default `60`) — length of each high/low bar.
- `--periods-per-year` (default `525600`, minute bars) — annualization multiplier.
- `--poll-secs` (default `5`) — mid-sampling cadence inside a period.
- One or more of `--stdout`, `--log-file <path>`, `--webhook <url>`.

## Emitted record

```json
{
  "kind": "volatility.parkinson",
  "ts": "2026-07-09T12:00:00Z",
  "market_id": "CC-USDC",
  "mid": 1.02,
  "period_secs": 60,
  "current_bar": { "high": 1.03, "low": 1.01 },
  "high": 1.05,
  "low":  0.99,
  "samples": 42,
  "sigma_per_period": 0.0021,
  "sigma_annualized":  0.28,
  "periods_per_year":  525600
}
```

`current_bar` reports the in-flight (not-yet-closed) bar. `high`/`low` are the aggregate max/min across the window of closed bars. Records with fewer than 2 closed bars carry `"warmup": true` instead.

## Metadata

| Field | Value |
|---|---|
| Category | `risk` |
| Agent slug | `volatility-screening` |
| Template | `Parkinson` (rust) |
| Status | `active` |
| Tags | `volatility`, `parkinson`, `high-low`, `range-based` |
| Required balance | None — read-only |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-volatility-screening-parkinson`
- Binary: `agent-volatility-screening-parkinson`

## Resources

- Live demo: <https://silvana.one/agents/demo/volatility-screening-parkinson/>
- Contact Us: <https://silvana.one/contact>
