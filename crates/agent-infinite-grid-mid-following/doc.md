# Infinite Grid — Mid-Following

> Grid market-maker with no fixed price range — regenerates a bid/offer ladder around the live mid on every refresh, so the strategy follows the market indefinitely.

## What it does

Regenerates a bid/offer ladder around the live mid every refresh, with no fixed price range. `--step-pct` is the spacing between adjacent levels and `--levels` the number per side. `--drift-threshold-pct` lets you avoid churning the book when the mid hasn't moved meaningfully since the previous build.

## How it works

Infinite Grid Agent — grid that follows the mid price

Unlike `agent-spot-grid` (which reads static `[[markets.bid_levels]]` /
`[[markets.offer_levels]]` from `agent.toml`), this agent regenerates its
ladder around the *current* mid every refresh cycle:

- Level i (1..=N) bid price  = mid × (1 − step_pct × i / 100)
- Level i (1..=N) offer price = mid × (1 + step_pct × i / 100)

Each refresh cancels existing own orders on the market and re-quotes the
whole ladder. There is no fixed price range — the grid drifts with the
market indefinitely. Useful for trending or wide-range conditions where a
static grid would have its outer levels left far from price.

## Usage

```bash
agent-infinite-grid-mid-following run --market CC-USDC --step-pct 0.5 --levels 5 --quantity-per-level 0.1
agent-infinite-grid-mid-following run --market CC-USDC --step-pct 0.25 --levels 10 --quantity-per-level 0.05 \
                       --refresh-secs 30 --drift-threshold-pct 0.1
agent-infinite-grid-mid-following status
```

## Metadata

| Field | Value |
|---|---|
| Category | `trading` |
| Agent slug | `infinite-grid` |
| Template | `Mid-Following` (rust) |
| Status | `active` |
| Tags | `grid`, `dynamic`, `market-making` |
| Required balance | $500 min |
| Supported assets | CC, USDC |
| Supported projects | Silvana |

## Package

- Crate: `agent-infinite-grid-mid-following`
- Binary: `agent-infinite-grid-mid-following`

## Resources

- Live demo: <https://silvana.one/agents/demo/infinite-grid-mid-following/>
- Contact Us: <https://silvana.one/contact>
