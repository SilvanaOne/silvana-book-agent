# Audit Retention — Chained

> Splits a Trading History JSONL into signed, hash-chained per-day (or per-week) slices. Enforces retention windows without breaking the audit chain.

## What it does

Splits a large `agent-trading-history` JSONL into per-day (or per-week with `--weekly`) slice files. Each slice starts with a signed header that embeds the `prev_slice_hash` of the previous slice — the audit chain remains provable across rotations. With `--retention-days N`, records older than the window are dropped; the surviving slice headers still chain through. `verify` walks the slice directory in chronological order and checks the header chain.

## How it works

Audit Retention Agent — chained log rotation

Reads a big `agent-trading-history` JSONL, splits it into per-day
(or per-week) slice files, and stitches the slices back into one
cross-slice chain by embedding each slice's `prev_slice_hash` in a
signed slice header record at the top of every slice.

Slices are named `slice-YYYY-MM-DD.jsonl` (or `slice-YYYY-Www.jsonl`
for weekly). Records older than `--retention-days` (if given) are
dropped — but the chain remains provable through the surviving slice
headers.

Two commands:
- `rotate` — read history, emit slices into `--out-dir`
- `verify` — walk the slice directory in chronological order and
  verify each slice's prev_slice_hash points at the previous slice's
  line-hash tail.

## Usage

```bash
agent-audit-retention-chained rotate --history history.jsonl --out-dir slices/
agent-audit-retention-chained rotate --history history.jsonl --out-dir slices/ --weekly --retention-days 90
agent-audit-retention-chained verify --dir slices/
```

## Metadata

| Field | Value |
|---|---|
| Category | `auditing` |
| Agent slug | `audit-retention` |
| Template | `Chained` (rust) |
| Status | `active` |
| Tags | `audit`, `retention`, `rotation`, `signed` |
| Required balance | None — offline |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-audit-retention-chained`
- Binary: `agent-audit-retention-chained`

## Resources

- Live demo: <https://silvana.one/agents/demo/audit-retention-chained/>
- Contact Us: <https://silvana.one/contact>
