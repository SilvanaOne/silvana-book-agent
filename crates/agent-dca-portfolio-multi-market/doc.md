# DCA Portfolio — Multi-Market

> Runs spot DCA in parallel across a basket of markets — same interval and per-tick amount applied to each market in the configured list.

## What it does

Same logic as `agent-spot-dca` but the order-placement loop is fanned out across every
market in `--markets`. Same `--amount` and `--interval` apply to each market.

## How it works

Spot DCA Agent — accumulate assets gradually at lower average cost

Places a limit buy (or sell) order at regular intervals on a spot market.
Settlement is handled automatically by the agent-core settlement executor.
All ledger operations are proxied through DAppProviderService (CIP-0103).

## Usage

```bash
agent-dca-portfolio-multi-market run --markets CBTC-CC,CETH-CC --amount 0.001 --interval 3600 --side buy
agent-dca-portfolio-multi-market run --markets CC-USDC --amount 10 --price-offset-pct -0.5 --max-total 1000
agent-dca-portfolio-multi-market status
```

## Metadata

| Field | Value |
|---|---|
| Category | `portfolio_mgmt` |
| Agent slug | `dca-portfolio` |
| Template | `Multi-Market` (rust) |
| Status | `active` |
| Tags | `dca`, `multi-market` |
| Required balance | $100 min |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-dca-portfolio-multi-market`
- Binary: `agent-dca-portfolio-multi-market`

## Resources

- Live demo: <https://silvana.one/agents/demo/dca-portfolio-multi-market/>
- Contact Us: <https://silvana.one/contact>
