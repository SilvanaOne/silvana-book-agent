# Smart Allocation — Buckets

> Rule-based two-level bucket portfolio — assets grouped into themed buckets with per-bucket target weights and dynamic sub-allocation between instruments.

## What it does

Smart Allocation Agent — two-level bucket portfolio

## How it works

Smart Allocation Agent — two-level bucket portfolio

Generalization of `agent-portfolio-rebalancing`: instruments are grouped
into *buckets* (e.g. "stables", "btc-themed", "long-duration"), each
bucket has a target weight in the overall portfolio, and inside a bucket
each instrument has its own within-bucket weight. Each cycle the agent:

1. Values every instrument at mid.
2. Aggregates per bucket; compares each bucket's current weight vs target.
3. Where the bucket itself is out of band, walks its instruments and places
   rebalance orders weighted by the within-bucket targets.

Policy TOML:

```toml
[[bucket]]
name = "stables"
weight = 0.5
  [[bucket.instrument]]
  instrument = "Amulet"
  market = "CC-USDC"
  weight = 1.0

[[bucket]]
name = "btc-theme"
weight = 0.5
  [[bucket.instrument]]
  instrument = "CBTC"
  market = "CBTC-CC"
  weight = 1.0
```

## Usage

```bash
agent-smart-allocation-buckets --config agent.toml run --policy allocation-policy.toml --bucket-threshold-pct 2.0 --rebalance-fraction 0.5 --check-interval 300 --no-restore
agent-smart-allocation-buckets --config agent.toml --dry-run run --policy portfolio.toml --bucket-threshold-pct 1.5 --rebalance-fraction 0.75 --check-interval 600
agent-smart-allocation-buckets --config agent.toml --verbose check --policy allocation-policy.toml
```

## Metadata

| Field | Value |
|---|---|
| Category | `portfolio_mgmt` |
| Agent slug | `smart-allocation` |
| Template | `Buckets` (rust) |
| Status | `active` |
| Tags | `allocation`, `buckets`, `themes` |
| Required balance | $1000 min |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-smart-allocation-buckets`
- Binary: `agent-smart-allocation-buckets`

## Resources

- Live demo: <https://silvana.one/agents/demo/smart-allocation-buckets/>
- Contact Us: <https://silvana.one/contact>
