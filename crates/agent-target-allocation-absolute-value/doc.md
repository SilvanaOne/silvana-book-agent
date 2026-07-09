# Target Allocation — Absolute Value

> Absolute quote-value targets per instrument — sibling of Portfolio Rebalancing but parametrized by amount, not by weight.

## What it does

Sibling of `agent-portfolio-rebalancing` parameterized by absolute quote-currency value per instrument. Example: keep 10000 USDC of Amulet and 20000 CC of CBTC at any time, regardless of the rest of the portfolio.

## How it works

Target Allocation Agent — absolute-value target balancer

Sibling of `agent-portfolio-rebalancing` parameterized by **absolute
quote-currency targets** rather than weights. Each `--target` says: keep
this instrument valued at this many quote-currency units (e.g. "hold 10
USDC worth of Amulet at any time"):

```
--target Amulet@CC-USDC=10000   # 10 000 USDC of Amulet via CC-USDC
--target CBTC@CBTC-CC=20000     # 20 000 CC of CBTC via CBTC-CC
```

Each cycle values each instrument at the live mid for its market and
computes the deviation from target. If `|deviation| > --threshold-quote`,
places a rebalancing order on that instrument's market for a quantity that
closes `--rebalance-fraction` of the deviation.

## Usage

```bash
agent-target-allocation-absolute-value run \
    --target Amulet@CC-USDC=10000 \
    --target CBTC@CBTC-CC=20000 \
    --threshold-quote 100 --rebalance-fraction 0.5
agent-target-allocation-absolute-value status
```

## Metadata

| Field | Value |
|---|---|
| Category | `portfolio_mgmt` |
| Agent slug | `target-allocation` |
| Template | `Absolute Value` (rust) |
| Status | `active` |
| Tags | `allocation`, `targets` |
| Required balance | $500 min |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-target-allocation-absolute-value`
- Binary: `agent-target-allocation-absolute-value`

## Resources

- Live demo: <https://silvana.one/agents/demo/target-allocation-absolute-value/>
- Contact Us: <https://silvana.one/contact>
