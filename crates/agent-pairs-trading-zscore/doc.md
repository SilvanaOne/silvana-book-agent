# Pairs Trading — Z-Score

> Rolling-window z-score stat-arb: enter opposite-direction orders per leg when the price ratio deviates by more than `entry_z` sample standard deviations from its rolling mean.

## What it does

Tracks the ratio `r = mid_A / mid_B` and maintains a rolling window of length `--window` (default 60). Each cycle computes the window mean μ and sample stddev σ, then evaluates `z = (r_now − μ) / σ`. When `|z|` exceeds `--entry-z` (default 2.0), the agent places one limit order per leg in opposite directions. Below `--exit-z` (default 0.5) the agent stays out — no new entries. Existing orders live/expire naturally; this variant does not actively manage exits (same minimal design as the ratio-divergence sibling).

## How it works

Pairs Trading Agent — z-score variant

Watches two markets A and B, samples `r = mid_A / mid_B` every `--poll-secs`, and
maintains a rolling window of the last `--window` samples. Once the window has at
least `--warmup-samples` values (default = window) and σ > 0 with ≥ 3 samples,
the agent computes:

```
μ = mean(window)
σ = sample_stddev(window)
z = (r_now − μ) / σ
```

Entry logic:

- `z > +entry_z` — A is relatively expensive vs. its rolling relationship with B:
    OFFER `--quantity-a` of A at mid_A, BID `--quantity-b` of B at mid_B
- `z < −entry_z` — A is relatively cheap:
    BID `--quantity-a` of A at mid_A, OFFER `--quantity-b` of B at mid_B
- `|z| ≤ exit_z` — do not enter (in-memory `position_open` flag is cleared)

Skips entry if an open own-order already exists in the intended direction on
either leg (fetched via `GetOrders`). At most one pair open at a time by design.
No stacking, no active exits — a starting point for adding a real cointegration
model or a Kalman-filtered spread.

## Usage

```bash
agent-pairs-trading-zscore run --market-a CC-USDC --market-b CC-USDT \
    --window 60 --entry-z 2.0 --exit-z 0.5 \
    --quantity-a 10 --quantity-b 10
```

Optional flags:

```bash
--poll-secs 30
--warmup-samples 60   # defaults to --window
--no-restore
```

## Metadata

| Field | Value |
|---|---|
| Category | `trading` |
| Agent slug | `pairs-trading` |
| Template | `Z-Score` (rust) |
| Status | `active` |
| Tags | `pairs-trading`, `zscore`, `stat-arb` |
| Required balance | $500 min |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-pairs-trading-zscore`
- Binary: `agent-pairs-trading-zscore`

## Resources

- Live demo: <https://silvana.one/agents/demo/pairs-trading-zscore/>
- Contact Us: <https://silvana.one/contact>
