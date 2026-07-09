# Liquidity Screening — Depth Probe

> Publishes spread, depth totals, and a VWAP-slippage probe per market on every poll — sizes up the book before you send an order.

## What it does

Polls `GetOrderbookDepth` per market and publishes spread / spread_bps, bid+offer depth totals, and a slippage probe: for a configurable `--probe-qty`, walks the book to compute VWAP-vs-mid in basis points.

## How it works

Liquidity Screening Agent

Polls `GetOrderbookDepth` for each configured market and publishes a
snapshot of liquidity metrics per tick:

- **spread** = best_offer − best_bid, **spread_bps** = spread / mid × 10000
- **bid_depth_qty / offer_depth_qty** = sum of quantities across visible levels
- **bid_depth_notional / offer_depth_notional** = sum of qty × price
- **slippage estimate** for a target market-buy of `--probe-qty`:
  walks the offer side level by level until the requested quantity is
  covered; reports the average fill price and the bps deviation from mid.
- mirror computation for a market-sell against the bid side.

Pure read-only screener — no ledger writes, no orders. Pair with the
trading agents to make liquidity-aware sizing decisions.

## Usage

```bash
agent-liquidity-screening-depth-probe run --markets CC-USDC,BTC-USD --probe-qty 10 --stdout
agent-liquidity-screening-depth-probe run --markets CC-USDC --probe-qty 50 --depth 30 --log-file liq.jsonl
agent-liquidity-screening-depth-probe probe --market CC-USDC --probe-qty 25
```

## Metadata

| Field | Value |
|---|---|
| Category | `risk` |
| Agent slug | `liquidity-screening` |
| Template | `Depth Probe` (rust) |
| Status | `active` |
| Tags | `depth`, `slippage`, `screening` |
| Required balance | None — read-only |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-liquidity-screening-depth-probe`
- Binary: `agent-liquidity-screening-depth-probe`

## Resources

- Live demo: <https://silvana.one/agents/demo/liquidity-screening-depth-probe/>
- Contact Us: <https://silvana.one/contact>
