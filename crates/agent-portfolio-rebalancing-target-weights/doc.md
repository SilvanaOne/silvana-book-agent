# Portfolio Rebalancing — Target Weights

> Maintains target weights across instruments. Each cycle values the portfolio in quote currency, compares against targets, and places one rebalance order per breach.

## What it does

Maintains target weights across instruments. Each cycle values your portfolio in quote currency via live mids, compares each instrument's current weight against target, and places a single rebalance order (BID under-weight, OFFER over-weight) on the configured market when the deviation exceeds `--threshold-pct`. `--rebalance-fraction` controls how aggressively each cycle closes the gap.

## How it works

Portfolio Rebalancing Agent — weight-based multi-asset balancer

Configure a target weight per instrument with `--target inst=weight`
(weights are normalized so they don't have to sum to 1). The agent:

1. Fetches unlocked balances per instrument and the live mid for the
   configured market (per instrument) → values each balance in the quote
   currency.
2. Computes `current_weight = inst_value / portfolio_value` and the
   deviation `Δ = current_weight − target_weight`.
3. If `|Δ| > --threshold-pct`, places a market-side order on the configured
   market to push the instrument back toward its target — selling when
   over-weight, buying when under-weight. Sized so the trade alone closes a
   fraction `--rebalance-fraction` of the gap (default 1.0).

Targets use a flat `INSTRUMENT@MARKET=WEIGHT` syntax to associate each
instrument with the market it should be rebalanced on (e.g.
`--target Amulet@CC-USDC=0.4`).

## Usage

```bash
agent-portfolio-rebalancing-target-weights run \
    --target Amulet@CC-USDC=0.4 \
    --target CBTC@CBTC-CC=0.6 \
    --threshold-pct 2.0 --rebalance-fraction 0.5

agent-portfolio-rebalancing-target-weights status
```

## Metadata

| Field | Value |
|---|---|
| Category | `portfolio_mgmt` |
| Agent slug | `portfolio-rebalancing` |
| Template | `Target Weights` (rust) |
| Status | `active` |
| Tags | `rebalance`, `target-weights` |
| Required balance | $500 min |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-portfolio-rebalancing-target-weights`
- Binary: `agent-portfolio-rebalancing-target-weights`

## Resources

- Live demo: <https://silvana.one/agents/demo/portfolio-rebalancing-target-weights/>
- Contact Us: <https://silvana.one/contact>
