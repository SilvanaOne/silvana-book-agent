# Copy Trading — Mirror

> Mirror a leader party's order flow onto your own book. Every leader `order.created` becomes a scaled mirror trade — filtered by market and capped by per-trade notional. Read-only observation of the leader; two-phase signing only on your own side.

## What it does

Copy Trading Agent — mirror a leader party's order flow

## How it works

Copy Trading Agent — mirror a leader party's order flow

Subscribes to a "leader" party's `SubscribeOrders` stream. For every
`order.created` the leader emits, mirrors the same side + price at a
configurable `--scale` (fraction of the leader's quantity) onto this
party's own book. Optional filters gate which markets are mirrored and
cap how large a single mirror trade can be.

This standalone binary uses a local JSONL log as the leader stream
(one order per line, same schema as agent-trading-history's payload)
so it can be exercised offline. In a real deployment the input would
come from OrderbookService.SubscribeOrders scoped to the leader party.

## Usage

```bash
agent-copy-trading-mirror run --leader-stream leader-orders.jsonl --scale 0.5 --markets CC-USDC,BTC-USD --max-leader-notional 50000 --max-mirror-notional 25000 --stdout --log-file mirror-log.jsonl
agent-copy-trading-mirror run --leader-stream orders.jsonl --scale 1.0 --markets CC-USDC --stdout --dry-run
agent-copy-trading-mirror run --leader-stream leader.jsonl --scale 0.25 --webhook https://api.example.com/trades --log-file trades.jsonl
```

## Metadata

| Field | Value |
|---|---|
| Category | `trading` |
| Agent slug | `copy-trading` |
| Template | `Mirror` (rust) |
| Status | `active` |
| Tags | `copy-trading`, `mirror`, `leader-follower` |
| Required balance | $100 min |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-copy-trading-mirror`
- Binary: `agent-copy-trading-mirror`

## Resources

- Live demo: <https://silvana.one/agents/demo/copy-trading-mirror/>
- Contact Us: <https://silvana.one/contact>
