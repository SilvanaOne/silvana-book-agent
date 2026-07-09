# Pairs Trading — Ratio Divergence

> Trades two related markets against each other when the price ratio diverges from a target — opposite-direction orders per leg, one live pair at a time.

## What it does

Tracks ratio `r = mid_A / mid_B`. When `r` diverges from `--target-ratio` by more than `--threshold-pct`, places one order per leg in opposite directions. Skips if a live order already exists in the intended direction on either leg. Minimal stat-arb skeleton — bring your own rolling mean/variance model for production use.

## How it works

Pairs Trading Agent — dual-market spread divergence

Watches two markets A and B simultaneously and computes their price ratio
`r = mid_A / mid_B`. The user supplies the equilibrium target ratio and a
divergence threshold (in percent). When the live ratio moves beyond the
threshold in either direction, the agent places a single limit order on
each leg:

- r > target × (1 + threshold/100) → A is rich, B is cheap:
    SELL `--quantity-a` of A at mid_A, BUY  `--quantity-b` of B at mid_B
- r < target × (1 − threshold/100) → A is cheap, B is rich:
    BUY  `--quantity-a` of A at mid_A, SELL `--quantity-b` of B at mid_B

At most one open position per direction (i.e. skip when an order in that
leg is already live). This is a minimal "stat-arb" skeleton — no rolling
covariance, no z-score, no per-side hedge sizing. Use it as a base to
extend with a real mean/variance model.

## Usage

```bash
agent-pairs-trading-ratio-divergence run --market-a CC-USDC --market-b CC-USDT \
    --target-ratio 1.0 --threshold-pct 1.0 --quantity-a 10 --quantity-b 10
```

## Metadata

| Field | Value |
|---|---|
| Category | `trading` |
| Agent slug | `pairs-trading` |
| Template | `Ratio Divergence` (rust) |
| Status | `active` |
| Tags | `pairs`, `stat-arb`, `dual-leg` |
| Required balance | $500 min |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-pairs-trading-ratio-divergence`
- Binary: `agent-pairs-trading-ratio-divergence`

## Resources

- Live demo: <https://silvana.one/agents/demo/pairs-trading-ratio-divergence/>
- Contact Us: <https://silvana.one/contact>
