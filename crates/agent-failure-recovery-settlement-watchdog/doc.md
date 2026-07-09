# Failure Recovery — Settlement Watchdog

> Watchdog for stale or failed settlement proposals — sweeps pending flows on a schedule and optionally cancels the related orders.

## What it does

Sweeps pending settlement proposals on a schedule. Surfaces PENDING proposals older than `--max-pending-age-secs` and any FAILED proposals. With `--cancel-related-orders` it also cancels any of the agent's own active orders linked to a stuck/failed proposal (via `OrderMatch.bid_order_id` / `offer_order_id`). Deep retry logic still lives in `settlement.rs` — this is a watchdog, not a replacement.

## How it works

Failure Recovery Agent

Periodically sweeps the party's settlement proposals and surfaces:
- PENDING settlements older than `--max-pending-age-secs`
- FAILED settlements (always reported)

The deep retry/rollback flow already lives inside `settlement.rs` (the
`SettlementExecutor` retries failed steps and the payment queue retries
fee payments). This agent is a *separate* watchdog meant to run alongside
a trading agent: it surfaces problems an operator should act on, and can
optionally call `CancelOrder` for stuck orders associated with a stale
proposal (matched by `bid_order_id` / `offer_order_id`).

Exits cleanly on Ctrl-C. No ledger writes (cancel_order is JWT-only).

## Usage

```bash
agent-failure-recovery-settlement-watchdog run --max-pending-age-secs 3600 --check-interval 60
agent-failure-recovery-settlement-watchdog run --max-pending-age-secs 1800 --cancel-related-orders
agent-failure-recovery-settlement-watchdog run --cancel-related-orders --dry-run
agent-failure-recovery-settlement-watchdog snapshot --max-pending-age-secs 600
```

## Metadata

| Field | Value |
|---|---|
| Category | `tx_flow` |
| Agent slug | `failure-recovery` |
| Template | `Settlement Watchdog` (rust) |
| Status | `active` |
| Tags | `watchdog`, `settlement`, `recovery` |
| Required balance | None — read-only |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-failure-recovery-settlement-watchdog`
- Binary: `agent-failure-recovery-settlement-watchdog`

## Resources

- Live demo: <https://silvana.one/agents/demo/failure-recovery-settlement-watchdog/>
- Contact Us: <https://silvana.one/contact>
