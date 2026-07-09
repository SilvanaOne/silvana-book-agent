# Trading History — Signed Chain-Log

> Builds a tamper-evident audit log — every order and settlement event is appended to a JSONL file as a hash-chained, Ed25519-signed record that can later be verified.

## What it does

Subscribes to own orders + settlements. Each event is written to a JSONL file as a signed, hash-chained record: every line carries `prev_hash` of the previous line plus an Ed25519 signature over `(seq, ts, prev_hash, kind, sha256(payload))`. Editing any line breaks the chain from that point on.

## How it works

Trading History Agent — append-only, signed event log

Subscribes to this party's order + settlement events and writes each event
to a JSONL file as a *signed* record:

```json
{
  "seq": 17,
  "ts": "2026-05-20T12:34:56.123Z",
  "prev_hash": "<hex sha256 of previous line>",
  "kind": "settlement.settled",
  "payload": { ... },
  "signature": "<base64 Ed25519 over canonical(seq||ts||prev_hash||kind||payload)>",
  "public_key": "<base64url Ed25519 public key>"
}
```

The `prev_hash` chains records into an append-only tamper-evident log —
editing any line invalidates the chain from that point on. Verification
can be performed offline with `agent-signature verify-canonical`.

## Usage

```bash
agent-trading-history-signed-chain run --history-file history.jsonl
agent-trading-history-signed-chain run --history-file history.jsonl --market CC-USDC --no-orders
agent-trading-history-signed-chain verify --history-file history.jsonl
```

## Metadata

| Field | Value |
|---|---|
| Category | `auditing` |
| Agent slug | `trading-history` |
| Template | `Signed Chain-Log` (rust) |
| Status | `active` |
| Tags | `audit-log`, `signed`, `hash-chain` |
| Required balance | None — read-only |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-trading-history-signed-chain`
- Binary: `agent-trading-history-signed-chain`

## Resources

- Live demo: <https://silvana.one/agents/demo/trading-history-signed-chain/>
- Contact Us: <https://silvana.one/contact>
