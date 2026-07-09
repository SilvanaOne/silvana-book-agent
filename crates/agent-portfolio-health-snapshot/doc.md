# Portfolio Health — Snapshot

> Human-readable snapshot — balances per instrument, open orders aggregated by market and side, pending settlement notional, and optional mid-price valuation of net inventory.

## What it does

Human-readable snapshot: balances per instrument, open orders aggregated by market and side, pending settlement notional, optional mid-price valuation of net open inventory.

## How it works

Portfolio Health Check Agent — aggregated balance + exposure report

Pulls balances from the ledger, active orders and pending settlement
proposals from the orderbook, current market mid-prices from the pricing
service, and renders a single human-readable snapshot:

- Balances per instrument (total / unlocked / locked)
- Open exposure per market (bid notional vs offer notional)
- Pending settlement count + total notional
- Approximate portfolio NAV in the quote currency of each market

## Usage

```bash
agent-portfolio-health-snapshot report
agent-portfolio-health-snapshot report --markets CC-USDC,BTC-USD
```

## Metadata

| Field | Value |
|---|---|
| Category | `portfolio_mgmt` |
| Agent slug | `portfolio-health` |
| Template | `Snapshot` (rust) |
| Status | `active` |
| Tags | `report`, `exposure` |
| Required balance | None — read-only |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-portfolio-health-snapshot`
- Binary: `agent-portfolio-health-snapshot`

## Resources

- Live demo: <https://silvana.one/agents/demo/portfolio-health-snapshot/>
- Contact Us: <https://silvana.one/contact>
