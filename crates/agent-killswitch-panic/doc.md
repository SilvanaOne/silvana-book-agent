# Killswitch — Panic

> Emergency stop — instantly cancels every open order on demand, or trips automatically when health checks (open-order count, failed settlements) breach configured limits.

## What it does

Two modes. `panic` is one-shot (cancel everything now and exit). `run` monitors a health
condition on a schedule and trips when violated, exiting with code 2 so a supervisor can act.

## How it works

Killswitch Agent — emergency stop

Two modes:
- `panic` — one-shot: cancel every active order this party has, then exit.
- `run` — monitor mode: poll a health condition on a schedule, and if it
  trips, fire the panic action and exit. Conditions supported:
    * `--max-rejects` consecutive failed settlements
    * `--min-cc-balance` for the unlocked Canton Coin balance dropping below

Read the trigger that fired from the exit code (0 = manual/clean, 2 = trip).

## Usage

```bash
agent-killswitch-panic panic                                                  # cancel-all now
agent-killswitch-panic panic --market CC-USDC
agent-killswitch-panic run --max-open-orders 200 --check-interval 15
agent-killswitch-panic run --max-failed-settlements 3 --max-open-orders 200
agent-killswitch-panic run --max-open-orders 200 --dry-run                    # log only
```

## Metadata

| Field | Value |
|---|---|
| Category | `risk` |
| Agent slug | `killswitch` |
| Template | `Panic` (rust) |
| Status | `active` |
| Tags | `emergency-stop`, `cancel-all` |
| Required balance | None — read-only |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-killswitch-panic`
- Binary: `agent-killswitch-panic`

## Resources

- Live demo: <https://silvana.one/agents/demo/killswitch-panic/>
- Contact Us: <https://silvana.one/contact>
