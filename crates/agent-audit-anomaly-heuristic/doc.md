# Audit Anomaly — Heuristic

> Offline scan of a Trading History log for statistical anomalies — stuck settlements, rapid cancel-recreate, layer clusters, cancel bursts right after fills.

## What it does

Offline scan over any `agent-trading-history` JSONL. Applies heuristics that catch patterns worth reviewing after the fact — `stuck_settlement` (fill without a matching settled/failed inside the window), `rapid_cancel` (create+cancel within ms → spoofing indicator), `layer_cluster` (many same-side orders on one market inside a tight price band), `fill_before_cancel_burst` (cancel burst right after a fill → churn indicator). Distinct from live `agent-market-abuse`: works on any historical log, no stream connection.

## How it works

Audit Anomaly Agent — post-hoc statistical scan over trading-history

Offline reader over an `agent-trading-history` JSONL file. Applies a
handful of heuristics that catch patterns worth reviewing after the
fact, without needing a live orderbook:

- **stuck_settlement** — a filled order whose associated proposal never
  settled inside `--settlement-window-secs`.
- **rapid_cancel** — an `order.created` and its `order.cancelled` land
  within `--rapid-cancel-window-ms` (spoofing indicator).
- **layer_cluster** — `--layer-threshold` open same-side orders on one
  market clustered within `--layer-band-pct` price band (layering
  indicator).
- **fill_before_cancel_burst** — a burst of `--burst-count` cancels
  inside `--burst-window-secs` right after a fill (churn indicator).

Distinct from live `agent-market-abuse`: works on any historical log,
any depth of history, without a stream connection.

## Usage

```bash
agent-audit-anomaly-heuristic --history history.jsonl
agent-audit-anomaly-heuristic --history history.jsonl --layer-threshold 8 --layer-band-pct 0.5 --output anomalies.jsonl
agent-audit-anomaly-heuristic --history history.jsonl --settlement-window-secs 1800 --burst-count 10
```

## Metadata

| Field | Value |
|---|---|
| Category | `auditing` |
| Agent slug | `audit-anomaly` |
| Template | `Heuristic` (rust) |
| Status | `active` |
| Tags | `audit`, `anomaly`, `post-hoc` |
| Required balance | None — offline |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-audit-anomaly-heuristic`
- Binary: `agent-audit-anomaly-heuristic`

## Resources

- Live demo: <https://silvana.one/agents/demo/audit-anomaly-heuristic/>
- Contact Us: <https://silvana.one/contact>
