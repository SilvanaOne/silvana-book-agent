# PnL Screening — Settlement-Stream

> Streams settlements and maintains per-market position, weighted-average cost basis, realized P&L, and live unrealized P&L. Snapshots emitted on a schedule.

## What it does

Streams settlement events and maintains per-market position, weighted-average cost basis, realized P&L, and live unrealized P&L. Snapshots emitted to stdout / JSONL file every `--snapshot-secs`.

## How it works

PnL Screening Agent

Subscribes to this party's settlement stream and accumulates per-market
position and weighted-average cost basis. After each settled trade it
recomputes:

- **position** (signed): + when the party was the buyer, − when seller.
- **cost basis (WAVG)** maintained only for the "long" side of a market —
  when position is positive, cost basis tracks the WAVG price paid for the
  current open inventory; when position is closed (=0) cost basis resets.
- **realized PnL** accumulated when selling out of a long position
  (`(sell_price − cost_basis) × matched_qty`). The mirror case for short
  positions is symmetrical and supported.
- **unrealized PnL** = `position × (mid − cost_basis)` recomputed every
  `--snapshot-secs` using the live mid price.

Read-only: no orders, no ledger writes. Snapshots logged to stdout or a
JSONL file.

## Usage

```bash
agent-pnl-screening-settlement-stream run --stdout
agent-pnl-screening-settlement-stream run --market CC-USDC --log-file pnl.jsonl --snapshot-secs 60
```

## Metadata

| Field | Value |
|---|---|
| Category | `risk` |
| Agent slug | `pnl-screening` |
| Template | `Settlement-Stream` (rust) |
| Status | `active` |
| Tags | `pnl`, `position`, `cost-basis` |
| Required balance | None — read-only |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-pnl-screening-settlement-stream`
- Binary: `agent-pnl-screening-settlement-stream`

## Resources

- Live demo: <https://silvana.one/agents/demo/pnl-screening-settlement-stream/>
- Contact Us: <https://silvana.one/contact>
