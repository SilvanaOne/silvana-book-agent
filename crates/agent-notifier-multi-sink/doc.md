# Notifier — Multi-Sink

> Fans out live order, settlement, and price events to any combination of stdout, JSONL file, or HTTP webhook. Each event is a single self-describing JSON record.

## What it does

Forwards events from `SubscribeOrders`, `SubscribeSettlements`, `StreamPrices` to one or
more sinks (stdout / JSONL append file / HTTP webhook). Each event is a single JSON object
with `{ "kind": "...", "ts": "...", "payload": {...} }`.

## How it works

Notifier Agent — push events from Silvana subscriptions to external sinks

Implements both **Notification** and **Hooks-and-Signals** from the Tier 1
roadmap (the descriptions are functionally identical). Subscribes to any
combination of `SubscribeOrders`, `SubscribeSettlements` and `StreamPrices`,
converts each event to a JSON payload, and dispatches it to one or more
sinks: stdout, append-only log file, and/or HTTP webhook (POST JSON).

## Usage

```bash
agent-notifier-multi-sink run --orders --settlements --stdout
agent-notifier-multi-sink run --orders --settlements --webhook https://hooks.example.com/sink
agent-notifier-multi-sink run --price-markets CC-USDC,BTC-USD --log-file events.jsonl
agent-notifier-multi-sink run --orders --market CC-USDC --webhook https://… --stdout
```

## Metadata

| Field | Value |
|---|---|
| Category | `data_analytics` |
| Agent slug | `notifier` |
| Template | `Multi-Sink` (rust) |
| Status | `active` |
| Tags | `events`, `webhook`, `notify` |
| Required balance | None — read-only |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-notifier-multi-sink`
- Binary: `agent-notifier-multi-sink`

## Resources

- Live demo: <https://silvana.one/agents/demo/notifier-multi-sink/>
- Contact Us: <https://silvana.one/contact>
