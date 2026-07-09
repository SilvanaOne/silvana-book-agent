# Order Expiry — TTL

> Periodically scans your active orders and cancels any whose age exceeds a configurable TTL — keeps the book free of stale quotes.

## What it does

Periodically scans this party's active orders and cancels any whose age exceeds `--max-age-secs`.
Uses `OrderbookService.GetOrders` + `CancelOrder` — no two-phase signing needed (JWT only).

## How it works

Order Expiry Agent — cancel stale orders by TTL

Periodically polls the agent's own active orders and cancels any that have
been resting on the book longer than `--max-age-secs`. Optional `--market`
filter restricts the scan to one market; otherwise all markets are scanned.

## Usage

```bash
agent-order-expiry-ttl run --max-age-secs 3600 --check-interval 60
agent-order-expiry-ttl run --max-age-secs 600  --market CC-USDC --dry-run
agent-order-expiry-ttl status                              # list active orders with age
agent-order-expiry-ttl status --market CC-USDC
```

## Metadata

| Field | Value |
|---|---|
| Category | `tx_flow` |
| Agent slug | `order-expiry` |
| Template | `TTL` (rust) |
| Status | `active` |
| Tags | `ttl`, `cleanup` |
| Required balance | None — read-only |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-order-expiry-ttl`
- Binary: `agent-order-expiry-ttl`

## Resources

- Live demo: <https://silvana.one/agents/demo/order-expiry-ttl/>
- Contact Us: <https://silvana.one/contact>
