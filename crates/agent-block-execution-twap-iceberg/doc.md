# Block Execution — TWAP x Iceberg

> TWAP × iceberg hybrid — walks a large parent order across the book in time slices while keeping visible on-book depth bounded per slice.

## What it does

Walks a large parent order across the book. Parent is split into `--time-slices` time slices; within each slice the visible quantity on the book is kept ≤ `--visible`. Unfilled remainder of a slice rolls into the next slice.

## How it works

Block Execution Agent — large private trade with controlled signaling

Combines TWAP (time-slicing) and Iceberg (per-slice hidden chunks) to walk
a large parent order through the book without leaking its full size:

- Parent quantity `--total` is divided into `--time-slices` equal slices.
- Each slice runs for `duration_secs / time_slices` seconds.
- Within a slice the agent maintains at most `--visible` quantity live on
  the book at the configured price. As the visible child order fills (or
  leaves the active set), it is replaced with the next chunk until the
  slice's quantity is exhausted or the slice's window ends.
- When the slice window expires, any remaining quantity rolls into the
  next slice — this avoids leaving big chunks on the book at end-of-slice.

This is essentially `agent-iceberg-execution` time-bounded per slice, so
you can guarantee a worst-case fill schedule while still hiding child
order size.

## Usage

```bash
agent-block-execution-twap-iceberg run --market CC-USDC --side buy --total 100 --price 1.02 \
    --time-slices 10 --duration-secs 3600 --visible 2
```

## Metadata

| Field | Value |
|---|---|
| Category | `tx_flow` |
| Agent slug | `block-execution` |
| Template | `TWAP x Iceberg` (rust) |
| Status | `active` |
| Tags | `twap`, `iceberg`, `block` |
| Required balance | $500 min |
| Supported assets | CC, USDC |
| Supported projects | Silvana |

## Package

- Crate: `agent-block-execution-twap-iceberg`
- Binary: `agent-block-execution-twap-iceberg`

## Resources

- Live demo: <https://silvana.one/agents/demo/block-execution-twap-iceberg/>
- Contact Us: <https://silvana.one/contact>
