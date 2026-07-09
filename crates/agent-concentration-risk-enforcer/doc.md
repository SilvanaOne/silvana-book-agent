# Concentration Risk — Enforcer

> Enforces min/max share-of-portfolio per instrument — cancels bids on breach of the upper band and offers on breach of the lower band.

## What it does

Enforcement variant of `agent-risk-exposure`. When an instrument's share-of-portfolio exceeds `--max-share-pct`, cancels the agent's BIDs on that market (no further accumulation). Symmetrically, when it drops below `--min-share-pct`, cancels OFFERs (no further depletion). `--dry-run` only logs.

## How it works

Concentration Risk Prevention Agent

Enforcement variant of `agent-risk-exposure`. Each cycle:

1. Values the portfolio in quote currency via balances × live mid for the
   configured markets.
2. Computes the share-of-portfolio for each instrument.
3. If any instrument exceeds `--max-share-pct`, cancels the agent's own
   open BIDs on that instrument's market (buying more would worsen the
   concentration). Symmetric guardrail at `--min-share-pct` cancels OFFERs
   on under-concentrated instruments (selling more would deplete it).
4. `--dry-run` only logs what would be cancelled.

## Usage

```bash
agent-concentration-risk-enforcer run --markets CC-USDC,BTC-USD --max-share-pct 60
agent-concentration-risk-enforcer run --markets CC-USDC --max-share-pct 50 --min-share-pct 10 --dry-run
agent-concentration-risk-enforcer snapshot --markets CC-USDC,BTC-USD --max-share-pct 50
```

## Metadata

| Field | Value |
|---|---|
| Category | `risk` |
| Agent slug | `concentration-risk` |
| Template | `Enforcer` (rust) |
| Status | `active` |
| Tags | `concentration`, `enforcement` |
| Required balance | None — read-only |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-concentration-risk-enforcer`
- Binary: `agent-concentration-risk-enforcer`

## Resources

- Live demo: <https://silvana.one/agents/demo/concentration-risk-enforcer/>
- Contact Us: <https://silvana.one/contact>
