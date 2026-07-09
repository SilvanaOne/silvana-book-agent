# Order Expiry — Price Drift

> Periodically scans your active orders and cancels any whose price has drifted too far from the current mid — keeps the book fresh instead of relying on a fixed TTL.

## What it does

Every `--check-interval` seconds, fetches this party's active orders (optionally filtered by `--market`), reads the current mid via `PricingService.GetPrice`, and computes the drift percentage `|order_price − mid| / mid × 100` for each order. Any order whose drift exceeds `--max-drift-pct` is cancelled via `OrderbookService.CancelOrder`.

No two-phase ledger signing is required — plain JWT-authenticated cancels only.

## How it works

Order Expiry Agent — cancel orders on price drift

The TTL sibling of this agent cancels orders based on age alone; this variant instead reacts to how far each resting quote is from live fair value. That means:

- Orders that were priced correctly and remain close to mid keep their place on the book (even if they have been resting for a long time).
- Orders that were priced correctly at submission but the market has since moved away from them are pulled quickly — regardless of age.
- `--dry-run` logs the drift and the cancel-decision without actually calling `CancelOrder`.

## Strategy

1. Fetch this party's active orders (`OrderbookService.GetOrders`), optionally filtered by `--market`.
2. Group them by market and, per market, fetch the current mid (`PricingService.GetPrice`).
3. For each order compute `drift_pct = |order.price − mid| / mid × 100`.
4. If `drift_pct > --max-drift-pct`, call `CancelOrder` (or log the decision under `--dry-run`).
5. Print a per-cycle summary of scanned / cancelled / skipped orders.

## Usage

```bash
agent-order-expiry-price-drift run --max-drift-pct 1.5 --check-interval 60
agent-order-expiry-price-drift run --max-drift-pct 0.75 --market CC-USDC --dry-run
agent-order-expiry-price-drift status                              # list active orders with current drift
agent-order-expiry-price-drift status --market CC-USDC
```

### CLI flags for `run`

| Flag | Meaning | Default |
|---|---|---|
| `--max-drift-pct` | Cancel orders whose `|price−mid|/mid` exceeds this % | required |
| `--check-interval` | Seconds between scans | `60` |
| `--market` | Restrict scan to one market (`BASE-QUOTE`) | all markets |
| `--dry-run` | Log the decision without cancelling | off |

## Metadata

| Field | Value |
|---|---|
| Category | `tx_flow` |
| Agent slug | `order-expiry` |
| Template | `Price Drift` (rust) |
| Status | `active` |
| Tags | `expiry`, `cancel`, `drift` |
| Required balance | None — read-only + cancels only |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-order-expiry-price-drift`
- Binary: `agent-order-expiry-price-drift`

## Resources

- Live demo: <https://silvana.one/agents/demo/order-expiry-price-drift/>
- Contact Us: <https://silvana.one/contact>
