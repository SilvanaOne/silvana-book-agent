# Order Matching — Trigger

> Trigger sniper — places a bid the moment best-offer drops through a level, or an offer the moment best-bid climbs through one. One open snipe per side.

## What it does

Streams the orderbook depth for one market. When `best_offer <= --buy-trigger`, places a BID at that offer for `--quantity`. When `best_bid >= --sell-trigger`, places an OFFER at that bid. One open snipe per side at a time.

## How it works

Order Matching Agent — snipe orders crossing a configured trigger

Subscribes to the orderbook depth stream for a single market and watches
the best bid / best offer. When:

- `best_offer <= --buy-trigger` → places a BID at `best_offer` for
  `--quantity` (we are willing to buy at or below the trigger).
- `best_bid  >= --sell-trigger` → places an OFFER at `best_bid` for
  `--quantity` (we are willing to sell at or above the trigger).

At most one open snipe per direction at a time — if a previous snipe is
still open, we skip until it fills or is cancelled.

## Usage

```bash
agent-order-matching-trigger run --market CC-USDC --buy-trigger 0.95 --quantity 5
agent-order-matching-trigger run --market CC-USDC --sell-trigger 1.10 --quantity 5
agent-order-matching-trigger run --market CC-USDC --buy-trigger 0.95 --sell-trigger 1.10 --quantity 5
```

## Metadata

| Field | Value |
|---|---|
| Category | `trading` |
| Agent slug | `order-matching` |
| Template | `Trigger` (rust) |
| Status | `active` |
| Tags | `trigger`, `snipe` |
| Required balance | $200 min |
| Supported assets | CC, USDC |
| Supported projects | Silvana |

## Package

- Crate: `agent-order-matching-trigger`
- Binary: `agent-order-matching-trigger`

## Resources

- Live demo: <https://silvana.one/agents/demo/order-matching-trigger/>
- Contact Us: <https://silvana.one/contact>
