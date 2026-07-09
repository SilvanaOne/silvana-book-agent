# Witnesses — Shell Hooks

> Subscribes to own orders + settlements and spawns a configurable shell command per matched event kind — event context passes to the child via env vars.

## What it does

Subscribes to own orders + settlements and spawns a shell command per matched event-type. Event metadata is exported via `SILVANA_EVENT`, `SILVANA_EVENT_TS`, `SILVANA_PROPOSAL_ID`, `SILVANA_ORDER_ID`, `SILVANA_MARKET_ID`, `SILVANA_STATUS` env vars.

## How it works

Witnesses Agent — event-driven external command launcher

Subscribes to this party's own settlement and order events. For each event
whose type matches a configured trigger, spawns an external shell command,
passing key fields via environment variables:

- SILVANA_EVENT = "settlement.settled" / "order.filled" / etc.
- SILVANA_EVENT_TS = ISO timestamp
- SILVANA_PROPOSAL_ID / SILVANA_ORDER_ID
- SILVANA_MARKET_ID
- SILVANA_STATUS (settlement only)

Run a script per event-type with `--on-<event>=<command>`. Commands are
executed asynchronously; failures are logged but do not abort the agent.

## Usage

```bash
agent-witnesses-shell-hooks run --on-settled '/scripts/notify-settled.sh'
agent-witnesses-shell-hooks run --on-failed '/scripts/page-oncall.sh' --on-cancelled 'echo $SILVANA_PROPOSAL_ID >> cancels.log'
agent-witnesses-shell-hooks run --on-order-filled '/scripts/record-fill.sh' --market CC-USDC
```

## Metadata

| Field | Value |
|---|---|
| Category | `tx_flow` |
| Agent slug | `witnesses` |
| Template | `Shell Hooks` (rust) |
| Status | `active` |
| Tags | `events`, `shell`, `witness` |
| Required balance | None — read-only |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-witnesses-shell-hooks`
- Binary: `agent-witnesses-shell-hooks`

## Resources

- Live demo: <https://silvana.one/agents/demo/witnesses-shell-hooks/>
- Contact Us: <https://silvana.one/contact>
