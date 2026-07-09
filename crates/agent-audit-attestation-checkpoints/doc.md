# Audit Attestation — Checkpoints

> Publishes signed checkpoints of a Trading History chain head. Any single checkpoint later proves that earlier records existed at that time — without needing the full log.

## What it does

Reads the current head of an `agent-trading-history` JSONL file (last record's `seq`, `ts`, line-hash), wraps it in a signed `{ts, head_seq, head_hash, party, signature}` checkpoint, and emits to any combination of stdout / append JSONL file / HTTP webhook. Auditors can later use any single checkpoint to prove that earlier records existed at that time — without needing the full log. `snapshot` is one-shot; `run --interval-secs N` publishes on a schedule until SIGINT.

## How it works

Audit Attestation Agent — checkpoint publisher for a trading-history log

Reads the current head of an `agent-trading-history` JSONL file (last
record's chain hash, its `seq` and `ts`), wraps it in a signed
`{ ts, party, head_seq, head_hash, signature }` checkpoint, and emits
that checkpoint to stdout / an append-only file / an HTTP webhook.

Auditors can later use any single checkpoint to prove that any earlier
record existed at that time — without having to see the full log.

Two modes:
- `snapshot` — one-shot: read the head, publish, exit.
- `run --interval-secs N` — publish a fresh checkpoint every N seconds
  until SIGINT.

## Usage

```bash
agent-audit-attestation-checkpoints snapshot --history history.jsonl --stdout --party party::alice
agent-audit-attestation-checkpoints run --history history.jsonl --interval-secs 300 \
                             --log-file checkpoints.jsonl \
                             --webhook https://hooks.example.com/attest
```

## Metadata

| Field | Value |
|---|---|
| Category | `auditing` |
| Agent slug | `audit-attestation` |
| Template | `Checkpoints` (rust) |
| Status | `active` |
| Tags | `audit`, `checkpoint`, `signed` |
| Required balance | None — offline |
| Supported assets | N/A — signing-only |
| Supported projects | Silvana |

## Package

- Crate: `agent-audit-attestation-checkpoints`
- Binary: `agent-audit-attestation-checkpoints`

## Resources

- Live demo: <https://silvana.one/agents/demo/audit-attestation-checkpoints/>
- Contact Us: <https://silvana.one/contact>
