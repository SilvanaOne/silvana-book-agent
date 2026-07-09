# Watchlist — Passive

> Track markets you care about — streams live prices and orderbook depth for a configurable list of pairs. Read-only, no ledger writes.

## What it does

Subscribes to external price feeds (`StreamPrices`) and Silvana internal orderbook depth
(`SubscribeOrderbook`) for a configurable set of markets and logs every update. No orders,
no settlement, no ledger writes — pure observability.

## How it works

Watchlist Agent — read-only market monitor

Subscribes to external price feeds (`StreamPrices`) and internal Silvana
orderbook depth (`SubscribeOrderbook`) for a configurable set of markets and
prints every update to the log. No orders, no settlement, no ledger writes.

## Usage

```bash
agent-watchlist-passive run --markets CC-USDC,BTC-USD --depth 10
agent-watchlist-passive run --markets CC-USDC --depth 20 --include-orderbook --include-trades
agent-watchlist-passive run --markets CC-USDC --no-prices                # depth only
agent-watchlist-passive run --markets CC-USDC --no-depth                 # external prices only
agent-watchlist-passive snapshot --markets CC-USDC,BTC-USD --depth 5     # one-off snapshot
agent-watchlist-passive markets                                          # list available markets
```

## Metadata

| Field | Value |
|---|---|
| Category | `data_analytics` |
| Agent slug | `watchlist` |
| Template | `Passive` (rust) |
| Status | `active` |
| Tags | `read-only`, `market-data` |
| Required balance | None — read-only |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-watchlist-passive`
- Binary: `agent-watchlist-passive`

## Resources

- Live demo: <https://silvana.one/agents/demo/watchlist-passive/>
- Contact Us: <https://silvana.one/contact>
