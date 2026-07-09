# Portfolio Rebalancing — Threshold Bands

> Event-driven rebalancer. Each instrument has an upper/lower band around its target weight; when the current weight breaches a band, one order is placed sized to bring the instrument all the way back to target. Otherwise, do nothing.

## What it does

Where the sibling `Target Weights` template continuously nudges the portfolio toward the target on every cycle, the `Threshold Bands` variant is **event-driven**. Each instrument has an upper and lower deviation band around its target weight. As long as the current weight stays inside `[target − lower_band, target + upper_band]`, the agent is idle. When the current weight breaches a band the agent places **one** rebalance order sized to bring the instrument fully back to target — a single-shot correction, not a fractional nudge.

## How it works

Configure a target weight per instrument with `--target INSTRUMENT@MARKET=WEIGHT`
(weights are normalized so they don't have to sum to 1). The agent:

1. Fetches unlocked balances per instrument and the live mid for the configured
   market → values each balance in the quote currency.
2. Computes `current_weight = inst_value / portfolio_value` and the deviation
   `Δ = current_weight − target_weight`, expressed in percentage points (pp).
3. If `Δ > +upper_band_pct` → places one **OFFER** on the instrument's market
   sized so that filling it moves `current_weight` back to the target.
4. If `Δ < −lower_band_pct` → places one **BID** sized the same way.
5. Inside the band → nothing.
6. If an open own-order in the intended direction already exists on the market,
   the cycle skips (no stacking).

Under the hood the sizing is `notional = |Δ| × portfolio_value` and
`qty = notional / mid`, i.e. it closes the whole gap. Contrast with
`Target Weights`, where `notional = |Δ| × rebalance_fraction × portfolio_value`
and the fraction is typically < 1.

## Usage

```bash
agent-portfolio-rebalancing-threshold-bands run \
    --target Amulet@CC-USDC=0.4 \
    --target CBTC@CBTC-CC=0.6 \
    --upper-band-pct 2.0 --lower-band-pct 2.0

agent-portfolio-rebalancing-threshold-bands run \
    --target Amulet@CC-USDC=0.5 --target CBTC@CBTC-CC=0.5 \
    --upper-band-pct 3.0 --lower-band-pct 1.5 \
    --price-offset-pct 0.1 --check-interval 300 --dry-run

agent-portfolio-rebalancing-threshold-bands status
```

## Metadata

| Field | Value |
|---|---|
| Category | `portfolio_mgmt` |
| Agent slug | `portfolio-rebalancing` |
| Template | `Threshold Bands` (rust) |
| Status | `active` |
| Tags | `portfolio`, `rebalancing`, `bands`, `threshold` |
| Required balance | $500 min |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-portfolio-rebalancing-threshold-bands`
- Binary: `agent-portfolio-rebalancing-threshold-bands`

## Resources

- Live demo: <https://silvana.one/agents/demo/portfolio-rebalancing-threshold-bands/>
- Contact Us: <https://silvana.one/contact>
