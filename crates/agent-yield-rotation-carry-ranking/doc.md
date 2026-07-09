# Yield Rotation — Carry Ranking

> Ranks markets by risk-adjusted carry (change / volume / spread weighting) and emits a rotation signal whenever the top-ranked market changes.

## What it does

`score = w_change × change_24h_pct + w_volume × log10(volume_24h) − w_spread × spread_pct`. Emits the live ranking per cycle and a `yield.rotation_signal` whenever the top-ranked market changes. Read-only — pair with `agent-batch-orders` / a script to actually move capital.

## How it works

Yield Rotation Agent

Ranks a basket of markets by a simple "yield score" and emits a rotation
signal whenever the top-ranked market changes. The score combines three
factors per market:

- **24h percentage change** (from GetPrice): the directional carry.
- **24h volume** (from GetMarketData): depth proxy for whether the carry
  is actually realizable at size.
- **spread penalty**: `(ask − bid) / mid`, subtracted from the score so
  markets with wide spreads (high effective cost) rank lower.

Score formula (configurable weights):
```
score = w_change × change_24h_pct
      + w_volume × log10(volume_24h)
      − w_spread × spread_pct
```

Each cycle publishes the current ranking. When the top market changes, an
additional `yield.rotation_signal` record is emitted. Downstream agents
(e.g. an automation script wired to `agent-batch-orders`) can act on those
signals to move capital. Read-only by itself — no ledger writes.

## Usage

```bash
agent-yield-rotation-carry-ranking run --markets CC-USDC,BTC-USD,CETH-CC --stdout
agent-yield-rotation-carry-ranking snapshot --markets CC-USDC,BTC-USD
```

## Metadata

| Field | Value |
|---|---|
| Category | `portfolio_mgmt` |
| Agent slug | `yield-rotation` |
| Template | `Carry Ranking` (rust) |
| Status | `active` |
| Tags | `yield`, `carry`, `rotation` |
| Required balance | None — read-only |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-yield-rotation-carry-ranking`
- Binary: `agent-yield-rotation-carry-ranking`

## Resources

- Live demo: <https://silvana.one/agents/demo/yield-rotation-carry-ranking/>
- Contact Us: <https://silvana.one/contact>
