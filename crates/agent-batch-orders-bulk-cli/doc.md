# Batch Orders — Bulk CLI

> One-shot bulk-order CLI — submit orders from a JSONL file, cancel filtered subsets by market or side, or cancel every open order in a single call.

## What it does

One-shot commands for bulk operations.

## How it works

Batch Order Management Agent — bulk submit / bulk cancel CLI

Three one-shot commands. None of them is a long-running daemon — they
finish, print a summary, and exit.

- `submit-batch --file orders.jsonl` — read a JSONL file of orders and
  submit each via `OrderbookService.SubmitOrder`. Empty lines and lines
  starting with `#` are ignored. Errors are logged but do not stop the
  batch (use `--abort-on-error` to opt in to fail-fast).
- `cancel-batch [--market <id>] [--side buy|sell]` — fetch own active
  orders, filter, cancel each via `CancelOrder`.
- `cancel-all` — alias for `cancel-batch` with no filter.

## Usage

```bash
# Submit a JSONL file of orders
agent-batch-orders-bulk-cli submit-batch --file orders.jsonl
agent-batch-orders-bulk-cli submit-batch --file orders.jsonl --abort-on-error

# Cancel filtered subsets
agent-batch-orders-bulk-cli cancel-batch --market CC-USDC --side buy
agent-batch-orders-bulk-cli cancel-batch --market CC-USDC
agent-batch-orders-bulk-cli cancel-all

# Run settlement worker if your batch caused fills
agent-batch-orders-bulk-cli settle
```

## Metadata

| Field | Value |
|---|---|
| Category | `portfolio_mgmt` |
| Agent slug | `batch-orders` |
| Template | `Bulk CLI` (rust) |
| Status | `active` |
| Tags | `batch`, `bulk`, `cli` |
| Required balance | $100 min |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-batch-orders-bulk-cli`
- Binary: `agent-batch-orders-bulk-cli`

## Resources

- Live demo: <https://silvana.one/agents/demo/batch-orders-bulk-cli/>
- Contact Us: <https://silvana.one/contact>
