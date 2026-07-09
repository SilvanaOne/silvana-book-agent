# Inventory Management — Band

> Keeps the unlocked balance of a chosen instrument inside a target band by placing market-driven rebalance orders — sibling of Cash Buffer for tradable inventory.

## What it does

Polls the unlocked balance of `--instrument` and pushes orders to keep it inside `[target − tolerance, target + tolerance]`. Unlike `agent-cash-buffer` (which uses `TransferCc`), this one rebalances by placing orders on `--market`.

## How it works

Inventory Management Agent

Keeps the unlocked balance of a single instrument inside the target band
`[target − tolerance, target + tolerance]`. On every poll cycle:

- If balance > target + tolerance → place an OFFER (sell) at the live mid
  for `chunk_size` to reduce inventory.
- If balance < target − tolerance → place a BID (buy) at the live mid for
  `chunk_size` to top up inventory.
- Otherwise → no action.

Unlike `agent-cash-buffer` (which uses `TransferCc` for Canton Coin), this
agent rebalances *trading* inventory by placing market-side orders.

## Usage

```bash
agent-inventory-mgmt-band run --market CC-USDC --instrument Amulet --target 100 --tolerance 20 --chunk-size 5
agent-inventory-mgmt-band run --market CC-USDC --instrument Amulet --target 100 --tolerance 5 \
                        --chunk-size 2 --check-interval 30 --price-offset-pct -0.1
agent-inventory-mgmt-band status
```

## Metadata

| Field | Value |
|---|---|
| Category | `trading` |
| Agent slug | `inventory-mgmt` |
| Template | `Band` (rust) |
| Status | `active` |
| Tags | `inventory`, `rebalance` |
| Required balance | $100 min |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-inventory-mgmt-band`
- Binary: `agent-inventory-mgmt-band`

## Resources

- Live demo: <https://silvana.one/agents/demo/inventory-mgmt-band/>
- Contact Us: <https://silvana.one/contact>
