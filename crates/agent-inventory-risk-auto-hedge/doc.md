# Inventory Risk — Auto-Hedge

> Watches directional inventory build-up in market-making books — emits soft/hard band signals and optionally auto-hedges the excess exposure.

## What it does

Inventory Risk Prevention Agent

## How it works

Inventory Risk Prevention Agent

Watches the unlocked balance of `--instrument` against a target band
`[target − soft_tolerance, target + soft_tolerance]`. Two layers:

- **Soft band**: when balance crosses `soft_tolerance` an `inventory.risk`
  signal is emitted (suggested hedge direction + size) but no order is
  placed.
- **Hard band**: when balance crosses `hard_tolerance` and `--auto-hedge`
  is set, the agent places an opposite-side limit order on the hedge
  market sized to bring balance back to target.

Difference vs `agent-hedging`: hedging fires on *every* deviation; this
agent has two thresholds, signals first, only forces a hedge at the hard
band — appropriate when the directional risk is the symptom of a separate
strategy that you'd rather let work itself out under normal conditions.

## Usage

```bash
agent-inventory-risk-auto-hedge --config agent.toml run --instrument Amulet --hedge-market CC-USDC --target-balance 1000 --soft-tolerance 100 --hard-tolerance 500 --auto-hedge --check-interval 60 --stdout --log-file risk.log
agent-inventory-risk-auto-hedge --config agent.toml --dry-run run --instrument CBTC --hedge-market CBTC-CC --target-balance 50 --soft-tolerance 10 --hard-tolerance 25 --stdout
agent-inventory-risk-auto-hedge --config agent.toml --verbose run --instrument CC --hedge-market CC-USDC --target-balance 5000 --soft-tolerance 500 --hard-tolerance 2000 --auto-hedge --webhook https://alerts.example.com
```

## Metadata

| Field | Value |
|---|---|
| Category | `risk` |
| Agent slug | `inventory-risk` |
| Template | `Auto-Hedge` (rust) |
| Status | `active` |
| Tags | `inventory`, `risk`, `auto-hedge` |
| Required balance | $100 min |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-inventory-risk-auto-hedge`
- Binary: `agent-inventory-risk-auto-hedge`

## Resources

- Live demo: <https://silvana.one/agents/demo/inventory-risk-auto-hedge/>
- Contact Us: <https://silvana.one/contact>
