# Iceberg Execution — Hidden Parent

> Splits a large parent order into visible chunks — only one chunk is on the book at a time so the true size of the parent stays hidden.

## What it does

Splits `--total` into successive `--visible` chunks. Each chunk is placed only after the previous chunk leaves the active book (filled or cancelled). At any moment the on-book footprint of this parent is ≤ `--visible`.

## How it works

Iceberg Execution Agent — small visible chunks driven by fills

Splits a `--total` order into successive `--visible` chunks. Unlike TWAP
(which is time-driven), iceberg is *fill-driven*: each chunk is placed
only after the previous one has been filled (or cancelled). At any moment
the visible quantity on the book is ≤ `--visible`, hiding the true size
of the parent order.

Implementation:
1. Place one chunk of `--visible` at the configured price.
2. Poll the order's status via `get_active_orders` at `--poll-secs`.
3. When the order is no longer ACTIVE/PARTIAL, mark `filled_qty +=
   (visible − remaining)` and place the next chunk.
4. Stop when `filled_qty >= total`.

## Usage

```bash
agent-iceberg-execution-hidden-parent run --market CC-USDC --side buy --total 100 --visible 1 --price 1.02
agent-iceberg-execution-hidden-parent run --market CC-USDC --side sell --total 50 --visible 2.5 --price 1.05 \
                            --max-runtime-secs 7200
```

## Metadata

| Field | Value |
|---|---|
| Category | `trading` |
| Agent slug | `iceberg-execution` |
| Template | `Hidden Parent` (rust) |
| Status | `active` |
| Tags | `iceberg`, `hidden-size`, `execution` |
| Required balance | $500 min |
| Supported assets | CC, USDC |
| Supported projects | Silvana |

## Package

- Crate: `agent-iceberg-execution-hidden-parent`
- Binary: `agent-iceberg-execution-hidden-parent`

## Resources

- Live demo: <https://silvana.one/agents/demo/iceberg-execution-hidden-parent/>
- Contact Us: <https://silvana.one/contact>
