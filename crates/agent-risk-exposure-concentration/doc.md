# Risk Exposure — Concentration

> Aggregated dashboard of portfolio value, open notional per market, pending settlement notional, and per-instrument concentration. Emits a snapshot on a schedule.

## What it does

Periodic snapshot of portfolio value (balances × live mids), open notional per market, pending settlement notional, and concentration share per instrument. Sets `concentration_warn` when any single instrument exceeds `--concentration-warn-pct`. Read-only.

## How it works

Risk Exposure Dashboard

Periodic snapshot of the party's risk posture across all configured
markets. Each cycle pulls:

- Unlocked balances per instrument (ledger)
- Active orders aggregated by market and side (open notional)
- Pending settlement proposals (pending notional)
- Live mid for each configured market (for valuation)

Computes:
- Portfolio value in quote currency
- Concentration: each instrument's share of portfolio value
- Top-concentration flag if any single instrument exceeds
  `--concentration-warn-pct`
- Open notional / portfolio ratio (leverage proxy)

Emits one JSONL record per cycle to stdout / file / webhook. Read-only:
does not place or cancel orders.

## Usage

```bash
agent-risk-exposure-concentration run --markets CC-USDC,BTC-USD --snapshot-secs 60 --stdout
agent-risk-exposure-concentration run --markets CC-USDC --log-file exposure.jsonl --concentration-warn-pct 70
agent-risk-exposure-concentration snapshot --markets CC-USDC,BTC-USD
```

## Metadata

| Field | Value |
|---|---|
| Category | `risk` |
| Agent slug | `risk-exposure` |
| Template | `Concentration` (rust) |
| Status | `active` |
| Tags | `exposure`, `dashboard` |
| Required balance | None — read-only |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-risk-exposure-concentration`
- Binary: `agent-risk-exposure-concentration`

## Resources

- Live demo: <https://silvana.one/agents/demo/risk-exposure-concentration/>
- Contact Us: <https://silvana.one/contact>
