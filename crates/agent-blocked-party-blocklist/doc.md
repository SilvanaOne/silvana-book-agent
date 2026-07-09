# Blocked Party — Blocklist

> Streams settlement events and flags any proposal whose buyer or seller appears in a plain-text blocklist file. Reloads the blocklist on a configurable interval — no restart required.

## What it does

Streams settlements; flags any proposal whose buyer or seller appears in a plain-text blocklist file. Reloads the blocklist every `--reload-secs` so an operator can add entries without restarting.

## How it works

Blocked Party Detection Agent

Watches this party's settlement stream. Each `SettlementProposal` carries
a `buyer` and `seller` party id; if either matches a blocklist entry, the
agent emits an alert to stdout / JSONL file / HTTP webhook.

The blocklist is a plain-text file, one party id per line, lines starting
with `#` are comments. The file is re-read every `--reload-secs` so an
operator can add entries live without restarting.

## Usage

```bash
agent-blocked-party-blocklist run --blocklist blocked.txt --stdout
agent-blocked-party-blocklist run --blocklist blocked.txt --webhook https://hooks.example.com/compliance --market CC-USDC
agent-blocked-party-blocklist check --blocklist blocked.txt
```

## Metadata

| Field | Value |
|---|---|
| Category | `compliance` |
| Agent slug | `blocked-party` |
| Template | `Blocklist` (rust) |
| Status | `active` |
| Tags | `blocklist`, `screening` |
| Required balance | None — read-only |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-blocked-party-blocklist`
- Binary: `agent-blocked-party-blocklist`

## Resources

- Live demo: <https://silvana.one/agents/demo/blocked-party-blocklist/>
- Contact Us: <https://silvana.one/contact>
