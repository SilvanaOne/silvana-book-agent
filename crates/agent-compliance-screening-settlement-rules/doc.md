# Compliance Screening — Settlement Rules

> Applies a TOML compliance policy (blocked pairs, per-party notional caps, allowed-counterparty whitelist, blocked markets) to the live settlement stream.

## What it does

Generalization of `agent-blocked-party`. Applies a TOML policy (blocked pairs, per-party rolling daily notional caps, allowed-counterparty whitelist, blocked markets) to live settlement events. Emits `compliance.reject` or `compliance.accept` per match.

## How it works

Compliance Screening Agent — rule-engine for settlement flows

Generalization of `agent-blocked-party`. Subscribes to the settlement
stream and evaluates each proposal against a TOML policy file:

```toml
# Block a specific counterparty pair (either direction)
[[blocked_pairs]]
a = "party-a-..."
b = "party-b-..."

# Per-counterparty rolling daily notional cap (quote currency)
[[party_caps]]
party = "party-x-..."
window_hours = 24
max_notional = "100000"

# Restrict who we'll settle with at all (whitelist; empty = no restriction)
[global]
allowed_counterparties = []
blocked_markets = ["XXX-YYY"]
```

For each settlement: emits one event `compliance.accept` or
`compliance.reject` with a list of rule hits, to stdout / JSONL file /
webhook. Does **not** cancel anything — pair with an enforcement agent
(`agent-killswitch`, `agent-witnesses`) if you want action.

## Usage

```bash
agent-compliance-screening-settlement-rules run --policy policy.toml --stdout
agent-compliance-screening-settlement-rules run --policy policy.toml --webhook https://hooks.example.com/compliance --emit-accepts
agent-compliance-screening-settlement-rules check --policy policy.toml
```

## Metadata

| Field | Value |
|---|---|
| Category | `compliance` |
| Agent slug | `compliance-screening` |
| Template | `Settlement Rules` (rust) |
| Status | `active` |
| Tags | `compliance`, `policy`, `settlements` |
| Required balance | None — read-only |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-compliance-screening-settlement-rules`
- Binary: `agent-compliance-screening-settlement-rules`

## Resources

- Live demo: <https://silvana.one/agents/demo/compliance-screening-settlement-rules/>
- Contact Us: <https://silvana.one/contact>
