# Human Approval — Queue

> File-backed approval queue — orders wait for a human sign-off before being signed and submitted; supports approve, reject, purge, and settle workflows.

## What it does

File-backed queue (JSONL). Upstream agents (or scripts) append orders via `enqueue --file`; an operator runs `list` / `approve <id>` / `reject <id>` / `purge`. Approved orders are signed and submitted via `SubmitOrder`. The `settle` subcommand runs a background settlement worker.

## How it works

Human Approval Agent — manual sign-off queue

File-backed approval queue for orders that need a human signature before
hitting the orderbook. The flow:

1. Some upstream agent (or a script) calls `enqueue --file orders.jsonl`
   which appends each line as a *pending* entry in the queue file.
2. An operator runs `list` to inspect the queue.
3. The operator runs `approve <id>` or `reject <id>`.
   - `approve` signs the order with the agent's Ed25519 key and submits it
     via `OrderbookService.SubmitOrder`; the entry is marked APPROVED.
   - `reject` marks the entry REJECTED and never submits anything.
4. `purge` drops APPROVED+REJECTED entries from the file.

Entry shape inside the queue file (one JSONL record per line):

```json
{"id":"...","ts_enqueued":"...","status":"pending|approved|rejected",
 "order":{"market":"...","side":"buy|sell","quantity":"...","price":"...","ref":"..."},
 "decided_at":null,"decided_by":null,"reason":null,"submit_order_id":null}
```

## Usage

```bash
agent-human-approval-queue enqueue --file orders.jsonl
agent-human-approval-queue list --status pending
agent-human-approval-queue approve --id ha-... --by alice --reason "ok per policy"
agent-human-approval-queue reject  --id ha-... --by bob   --reason "exceeds risk budget"
agent-human-approval-queue purge
agent-human-approval-queue settle
```

## Metadata

| Field | Value |
|---|---|
| Category | `compliance` |
| Agent slug | `human-approval` |
| Template | `Queue` (rust) |
| Status | `active` |
| Tags | `approval`, `queue`, `manual` |
| Required balance | $100 min |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-human-approval-queue`
- Binary: `agent-human-approval-queue`

## Resources

- Live demo: <https://silvana.one/agents/demo/human-approval-queue/>
- Contact Us: <https://silvana.one/contact>
