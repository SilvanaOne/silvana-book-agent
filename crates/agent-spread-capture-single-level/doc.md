# Spread Capture — Single-Level

> Single-level two-sided market making — keeps one bid and one offer at mid ± half-spread, skipping a side when net inventory leaves the configured band.

## What it does

Keeps one bid and one offer at `mid × (1 ∓ spread_bps/20000)`. Each refresh cycle cancels existing orders, recomputes prices, and re-quotes — skipping the bid side when net inventory > `+max-inventory`, the offer side when < `-max-inventory`.

## How it works

Spread Capture Agent — single-level two-sided market maker

Keeps one bid and one offer near the mid price at all times. Each refresh
cycle:
1. Cancel any existing own orders on the market.
2. Compute new bid/offer at `mid × (1 ∓ spread_bps / 20000)`.
3. Skip the bid side if net inventory (cumulative buys - cumulative sells
   since process start) exceeds `--max-inventory`; skip the offer side if
   inventory drops below `−max-inventory`.
4. Submit the surviving side(s) sized at `--quantity`.

Inventory accounting is process-local — restart resets it to zero.

## Usage

```bash
agent-spread-capture-single-level run --market CC-USDC --spread-bps 25 --quantity 0.5 --max-inventory 10
agent-spread-capture-single-level run --market CC-USDC --spread-bps 50 --quantity 1.0 --max-inventory 5 --refresh-secs 15
```

## Metadata

| Field | Value |
|---|---|
| Category | `trading` |
| Agent slug | `spread-capture` |
| Template | `Single-Level` (rust) |
| Status | `active` |
| Tags | `marketmaker`, `inventory-clamp` |
| Required balance | $200 min |
| Supported assets | CC, USDC |
| Supported projects | Silvana |

## Package

- Crate: `agent-spread-capture-single-level`
- Binary: `agent-spread-capture-single-level`

## Resources

- Live demo: <https://silvana.one/agents/demo/spread-capture-single-level/>
- Contact Us: <https://silvana.one/contact>
