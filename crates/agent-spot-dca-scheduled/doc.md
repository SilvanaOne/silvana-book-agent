# Spot DCA — Scheduled

> Accumulate assets gradually at a lower average cost by placing periodic spot orders on a fixed schedule.

## What it does

Accumulate assets gradually at lower average cost on spot market.

## How it works

Spot DCA Agent — accumulate assets gradually at lower average cost

Places a limit buy (or sell) order at regular intervals on a spot market.
Settlement is handled automatically by the agent-core settlement executor.
All ledger operations are proxied through DAppProviderService (CIP-0103).

## Usage

```bash
agent-spot-dca-scheduled run --market CC-USDC --amount 10 --interval 3600 --side buy
agent-spot-dca-scheduled run --market CC-USDC --amount 10 --interval 3600 --side buy --price-offset-pct -0.5
agent-spot-dca-scheduled run --market CC-USDC --amount 10 --interval 3600 --max-total 1000
agent-spot-dca-scheduled status
```

## Metadata

| Field | Value |
|---|---|
| Category | `trading` |
| Agent slug | `spot-dca` |
| Template | `Scheduled` (rust) |
| Status | `active` |
| Tags | `dca`, `scheduler` |
| Required balance | $100 min |
| Supported assets | CC, USDC |
| Supported projects | Silvana |

## Package

- Crate: `agent-spot-dca-scheduled`
- Binary: `agent-spot-dca-scheduled`

## Resources

- Live demo: <https://silvana.one/agents/demo/spot-dca-scheduled/>
- Contact Us: <https://silvana.one/contact>
