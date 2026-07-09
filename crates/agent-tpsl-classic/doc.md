# Automated TP/SL — Classic

> Watch a live position and trigger take-profit or stop-loss exits automatically, with optional trailing stops.

## What it does

Take profit and stop loss with optional trailing stop.

## How it works

Automated TP/SL Agent — Take Profit and Stop Loss

Monitors price and automatically places orders when TP or SL levels are hit.
- Long position: TP sells when price >= tp_price, SL sells when price <= sl_price
- Short position: TP buys when price <= tp_price, SL buys when price >= sl_price
- Optional trailing stop: SL follows price upward (long) or downward (short)
Settlement is handled automatically by agent-core.

## Usage

```bash
agent-tpsl-classic run --market CC-USDC --side long --quantity 10 --tp 1.5 --sl 1.2
agent-tpsl-classic run --market CC-USDC --side long --quantity 10 --tp 1.5 --sl 1.2 --trailing-pct 2.0
agent-tpsl-classic run --market CC-USDC --side short --quantity 10 --tp 1.0 --sl 1.4
agent-tpsl-classic status
```

## Metadata

| Field | Value |
|---|---|
| Category | `trading` |
| Agent slug | `tpsl` |
| Template | `Classic` (rust) |
| Status | `active` |
| Tags | `take-profit`, `stop-loss`, `trailing` |
| Required balance | $100 min |
| Supported assets | CC, USDC |
| Supported projects | Silvana |

## Package

- Crate: `agent-tpsl-classic`
- Binary: `agent-tpsl-classic`

## Resources

- Live demo: <https://silvana.one/agents/demo/tpsl-classic/>
- Contact Us: <https://silvana.one/contact>
