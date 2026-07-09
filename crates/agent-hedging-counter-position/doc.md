# Hedging — Counter-Position

> Pushes the balance of a target instrument back toward a configured level by placing counter-directional orders sized against the deviation.

## What it does

Pushes the unlocked balance of `--exposure-instrument` back toward `--target-balance`. On every cycle: balance > target+tolerance → OFFER on hedge market; balance < target−tolerance → BID. Sized by `--hedge-fraction` of the deviation. Sibling of `agent-inventory-mgmt` but oriented around neutralizing unwanted exposure rather than maintaining a trading book.

## How it works

Hedging Agent — push directional exposure back to a target

Tracks the unlocked balance of `--exposure-instrument` against a
`--target-balance`. When the deviation exceeds `--tolerance`, places one
offsetting limit order on `--hedge-market`:

- balance > target + tolerance  → OFFER on hedge market (sell the surplus)
- balance < target − tolerance  → BID on hedge market (buy back to target)

Sized by `--hedge-fraction` of the deviation per cycle (so the agent walks
the exposure back rather than slamming the book in one go). Only one open
hedge per direction at a time.

This is the "counter-position" sibling of `agent-inventory-mgmt`. The
difference is intent: inventory-mgmt rebalances trading inventory you
actively want to hold; hedging neutralizes unwanted directional risk that
built up from other strategies.

## Usage

```bash
agent-hedging-counter-position run \
    --exposure-instrument Amulet --hedge-market CC-USDC \
    --target-balance 50 --tolerance 5 --hedge-fraction 0.5
```

## Metadata

| Field | Value |
|---|---|
| Category | `trading` |
| Agent slug | `hedging` |
| Template | `Counter-Position` (rust) |
| Status | `active` |
| Tags | `hedging`, `exposure` |
| Required balance | $200 min |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-hedging-counter-position`
- Binary: `agent-hedging-counter-position`

## Resources

- Live demo: <https://silvana.one/agents/demo/hedging-counter-position/>
- Contact Us: <https://silvana.one/contact>
