# Anomaly Classifier — Rule-Clustering

> Clusters audit-anomaly records into abuse categories (spoofing / wash_trading / stuck_settlement / normal_volatility) via a rule-based co-occurrence classifier. Drop-in ML model behind the same interface for production.

## What it does

Anomaly Classifier Agent

## How it works

Anomaly Classifier Agent

Consumes the output stream of `agent-audit-anomaly` (JSONL of anomaly
records, one per line) and classifies each into one of a small set of
trading-abuse buckets:

  - `spoofing`         — layer_cluster + rapid_cancel + fill_before_cancel_burst
  - `wash_trading`     — repeat market pair with tight fills matching cancels
  - `stuck_settlement` — unresolved settlement (pass-through, high-priority)
  - `normal_volatility` — anything else

Deterministic rule-based classifier here; swap in an ML model behind
the same interface in prod.

## Usage

```bash
agent-anomaly-classifier-rule-clustering classify --input anomalies.jsonl --output classified.jsonl --window-secs 60 --cluster-threshold 3
agent-anomaly-classifier-rule-clustering classify --input anomalies.jsonl --stdout --window-secs 120 --cluster-threshold 5
agent-anomaly-classifier-rule-clustering classify --input audit-output.jsonl --output results.jsonl
```

## Metadata

| Field | Value |
|---|---|
| Category | `intelligence` |
| Agent slug | `anomaly-classifier` |
| Template | `Rule-Clustering` (rust) |
| Status | `active` |
| Tags | `ml`, `clustering`, `audit` |
| Required balance | None — offline |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-anomaly-classifier-rule-clustering`
- Binary: `agent-anomaly-classifier-rule-clustering`

## Resources

- Live demo: <https://silvana.one/agents/demo/anomaly-classifier-rule-clustering/>
- Contact Us: <https://silvana.one/contact>
