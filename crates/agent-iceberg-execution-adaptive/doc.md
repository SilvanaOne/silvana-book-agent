# Iceberg Execution — Adaptive

> Splits a large parent order into visible chunks whose size adapts to how fast the previous chunk cleared.

## What it does

Same fill-driven iceberg pattern as the hidden-parent sibling — at most one child chunk on the book at a time, and chunk N+1 lands only after chunk N leaves the active book (filled or cancelled). Instead of a fixed `--visible` quantity, each next chunk is **resized** based on the fill velocity of the previous one.

## How it works

Adaptive Iceberg Agent — visible size follows recent fill velocity.

The parent order is worked chunk-by-chunk. For every completed chunk the loop
records `elapsed = last_chunk_end_at − last_chunk_start_at` and adjusts the
next visible quantity before the next child order is submitted:

- `elapsed < --fast-secs`  → next visible = `min(current × 2, --max-visible)`
  (the market is absorbing our size quickly — lean in)
- `elapsed > --slow-secs`  → next visible = `max(current / 2, --min-visible)`
  (fills are stalling — reduce the footprint we expose)
- Otherwise the next visible is unchanged.

Implementation:
1. Start with `--initial-visible`. Place one chunk at `--price`.
2. Poll the order's status via `get_active_orders` every `--poll-secs`.
3. When the order is no longer ACTIVE/PARTIAL, mark
   `filled_qty += (visible − remaining)`, compute the new visible from the
   elapsed window, and place the next chunk.
4. Stop when `filled_qty >= --total` or `--max-runtime-secs` elapses.

## Usage

```bash
agent-iceberg-execution-adaptive run \
    --market CC-USDC --side buy \
    --total 100 \
    --initial-visible 1 --min-visible 0.25 --max-visible 5 \
    --fast-secs 15 --slow-secs 90 \
    --price 1.02

agent-iceberg-execution-adaptive run \
    --market CC-USDC --side sell \
    --total 50 \
    --initial-visible 2 --min-visible 0.5 --max-visible 10 \
    --fast-secs 20 --slow-secs 120 \
    --price 1.05 --max-runtime-secs 7200
```

## Metadata

| Field | Value |
|---|---|
| Category | `trading` |
| Agent slug | `iceberg-execution` |
| Template | `Adaptive` (rust) |
| Status | `active` |
| Tags | `iceberg`, `adaptive`, `execution` |
| Required balance | $500 min |
| Supported assets | CC, USDC |
| Supported projects | Silvana |

## Package

- Crate: `agent-iceberg-execution-adaptive`
- Binary: `agent-iceberg-execution-adaptive`

## Resources

- Live demo: <https://silvana.one/agents/demo/iceberg-execution-adaptive/>
- Contact Us: <https://silvana.one/contact>
