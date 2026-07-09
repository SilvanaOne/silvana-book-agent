# Cash Buffer — Push-Only

> Keep your unlocked Canton Coin balance inside a band — excess is auto-transferred to a sink party, low balance raises a warning.

## What it does

Polls the agent's unlocked Canton Coin balance. When it exceeds `--max-cc`, the excess is
pushed to `--sink-party` via `TransferCc` (two-phase signed). Under-balance is logged as a
warning — agents can only PUSH, not PULL.

## How it works

Cash Buffer Agent — keep CC balance within a target band

Polls the Canton Coin balance on a schedule. If unlocked CC exceeds
`--max-cc`, the excess is transferred to `--sink-party` via `TransferCc`
(the agent has approval to PUSH but cannot PULL — so under-balance only
logs a warning rather than refilling).

Uses the same CIP-0103 two-phase signing pipeline as the other ledger-
writing agents (Spot DCA, TP/SL): boilerplate `ledger_client.rs`,
`amulet_cache.rs`, `acs_worker.rs`, `payment_queue.rs` are copied as-is.

## Usage

```bash
agent-cash-buffer-push-only run --min-cc 20 --max-cc 100 --sink-party <party-id> --check-interval 60
agent-cash-buffer-push-only run --min-cc 20 --max-cc 100 --sink-party <party-id> --dry-run
agent-cash-buffer-push-only status
```

## Metadata

| Field | Value |
|---|---|
| Category | `portfolio_mgmt` |
| Agent slug | `cash-buffer` |
| Template | `Push-Only` (rust) |
| Status | `active` |
| Tags | `treasury`, `transfer-cc` |
| Required balance | $50 min |
| Supported assets | CC |
| Supported projects | Silvana |

## Package

- Crate: `agent-cash-buffer-push-only`
- Binary: `agent-cash-buffer-push-only`

## Resources

- Live demo: <https://silvana.one/agents/demo/cash-buffer-push-only/>
- Contact Us: <https://silvana.one/contact>
