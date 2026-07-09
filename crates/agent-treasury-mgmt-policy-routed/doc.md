# Treasury Management — Policy-Routed

> Policy + approval-routed rebalancer — per-trade ceiling, rolling 24 h notional cap, and an approval threshold above which trades queue for human sign-off.

## What it does

Same target structure as `agent-target-allocation` (per-instrument absolute quote-value targets), with a treasury policy on top: per-trade ceiling, rolling 24h cap, and an approval threshold above which the trade goes into an `agent-human-approval` queue file instead of straight to the book.

## How it works

Treasury Management Agent

Policy-driven rebalancer with an approval workflow. Same target structure
as `agent-target-allocation` (per-instrument absolute quote-value targets),
but trades whose notional exceeds `--approval-threshold-quote` are
**enqueued** to a file consumable by `agent-human-approval` instead of
being submitted directly. Smaller trades go straight to the book.

Additional treasury controls:
- `--max-trade-quote` — absolute ceiling per individual trade, regardless
  of approval routing.
- `--max-daily-trade-quote` — rolling 24h cap across all rebalances.

## Usage

```bash
agent-treasury-mgmt-policy-routed run \
    --target Amulet@CC-USDC=10000 \
    --target CBTC@CBTC-CC=20000 \
    --approval-threshold-quote 1000 \
    --max-trade-quote 5000 \
    --max-daily-trade-quote 25000 \
    --approval-queue approval-queue.jsonl
```

## Metadata

| Field | Value |
|---|---|
| Category | `portfolio_mgmt` |
| Agent slug | `treasury-mgmt` |
| Template | `Policy-Routed` (rust) |
| Status | `active` |
| Tags | `treasury`, `policy`, `approval` |
| Required balance | $1000 min |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-treasury-mgmt-policy-routed`
- Binary: `agent-treasury-mgmt-policy-routed`

## Resources

- Live demo: <https://silvana.one/agents/demo/treasury-mgmt-policy-routed/>
- Contact Us: <https://silvana.one/contact>
