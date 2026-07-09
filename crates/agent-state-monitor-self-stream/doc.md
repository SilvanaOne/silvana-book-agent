# State Monitor — Self-Stream

> Observe your own orders and settlement proposals in real time — created, filled, cancelled, settled, failed — across every market.

## What it does

Read-only observer of the party's own state. Streams `SubscribeOrders` and `SubscribeSettlements`
in parallel and prints every event (created/filled/cancelled, proposal/settled/failed).

## How it works

State Monitoring Agent — observe this party's own orders and settlements

Read-only. Subscribes to `SubscribeOrders` and `SubscribeSettlements` and
prints every event (created/filled/cancelled/etc., proposal/settled/failed)
as a formatted log line. Useful for ops dashboards, audit trails, and
debugging settlement issues. No ledger writes.

## Usage

```bash
agent-state-monitor-self-stream run                                # all markets, both streams
agent-state-monitor-self-stream run --market CC-USDC
agent-state-monitor-self-stream run --no-settlements               # orders only
agent-state-monitor-self-stream snapshot --market CC-USDC          # one-off snapshot of orders + proposals
```

## Metadata

| Field | Value |
|---|---|
| Category | `data_analytics` |
| Agent slug | `state-monitor` |
| Template | `Self-Stream` (rust) |
| Status | `active` |
| Tags | `read-only`, `subscriptions` |
| Required balance | None — read-only |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-state-monitor-self-stream`
- Binary: `agent-state-monitor-self-stream`

## Resources

- Live demo: <https://silvana.one/agents/demo/state-monitor-self-stream/>
- Contact Us: <https://silvana.one/contact>
