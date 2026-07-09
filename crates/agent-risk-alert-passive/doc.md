# Risk Alert — Passive

> Passive risk monitor — polls orderbook state and fires alerts when thresholds (open orders, failed settlements, notional caps) are breached. Does not cancel anything.

## What it does

Polls orderbook state and emits alerts when thresholds breach. Does NOT cancel orders — pair it with `agent-killswitch` if you also want enforcement.

## How it works

Risk Alert Agent — passive threshold monitor

Polls orderbook state on a schedule and emits an alert when any configured
threshold is breached. Unlike `agent-killswitch`, this agent does NOT cancel
orders or stop trading — it only notifies.

Triggers (any combination):
- `--max-open-orders` — total active orders across all markets
- `--max-failed-settlements` — count of pending proposals with FAILED status
- `--max-open-notional` — sum of price × remaining_quantity across all open orders

Sinks: any combination of stdout, append JSONL file, HTTP webhook (POST).
At least one sink must be configured.

## Usage

```bash
agent-risk-alert-passive run --max-open-orders 200 --max-failed-settlements 3 --stdout
agent-risk-alert-passive run --max-open-notional 100000 --webhook https://hooks.example.com/sink
agent-risk-alert-passive run --max-open-orders 100 --log-file alerts.jsonl --check-interval 60
agent-risk-alert-passive check --max-open-orders 100        # one-off (non-zero exit on breach)
```

## Metadata

| Field | Value |
|---|---|
| Category | `risk` |
| Agent slug | `risk-alert` |
| Template | `Passive` (rust) |
| Status | `active` |
| Tags | `alerts`, `monitoring` |
| Required balance | None — read-only |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-risk-alert-passive`
- Binary: `agent-risk-alert-passive`

## Resources

- Live demo: <https://silvana.one/agents/demo/risk-alert-passive/>
- Contact Us: <https://silvana.one/contact>
