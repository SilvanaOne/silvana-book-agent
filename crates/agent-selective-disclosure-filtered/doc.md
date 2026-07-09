# Selective Disclosure — Filtered

> Post-processor over a Trading History log — filters by event kind, market, and redact-fields, then writes a fresh hash-chained signed log of only what you intend to share.

## What it does

Post-processor over an `agent-trading-history` JSONL file. Filters by event kind / market / redact-fields and writes a fresh hash-chained signed log of only the records you intend to share. Recipients can verify it with either `agent-selective-disclosure verify` or `agent-trading-history verify` — both share the same record schema.

## How it works

Selective Disclosure Agent

Post-processor over an `agent-trading-history` JSONL log. Reads the signed,
hash-chained log, applies filters (event-kind / market / field redaction),
and emits a **fresh** hash-chained signed log that contains only what you
intend to share with an auditor or regulator.

Two modes:
- `filter` — produce the disclosure log
- `verify` — independently verify chain + signatures of a disclosure log

The disclosure log uses the same record schema as `agent-trading-history`
(`seq, ts, prev_hash, kind, payload, payload_sha256, signing_scheme,
signature, public_key`) and signs every line with this party's Ed25519 key.
That means a recipient can verify the chain locally with the standard
`agent-trading-history verify` if they wish.

## Usage

```bash
agent-selective-disclosure-filtered filter --history history.jsonl --output disclosure.jsonl \
    --kinds settlement.settled,order.filled --markets CC-USDC \
    --redact-fields buyer,seller
agent-selective-disclosure-filtered verify --output disclosure.jsonl
```

## Metadata

| Field | Value |
|---|---|
| Category | `auditing` |
| Agent slug | `selective-disclosure` |
| Template | `Filtered` (rust) |
| Status | `active` |
| Tags | `audit`, `redaction`, `signed` |
| Required balance | None — offline |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-selective-disclosure-filtered`
- Binary: `agent-selective-disclosure-filtered`

## Resources

- Live demo: <https://silvana.one/agents/demo/selective-disclosure-filtered/>
- Contact Us: <https://silvana.one/contact>
