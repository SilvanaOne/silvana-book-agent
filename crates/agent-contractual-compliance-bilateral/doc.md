# Contractual Compliance — Bilateral

> Evaluates bilateral contractual obligations against live activity — flags any order or settlement that violates a party's contract terms.

## What it does

Contractual Compliance Agent

## How it works

Contractual Compliance Agent

Tracks settled flows against a TOML file of bilateral contracts. Each
contract entry declares a counterparty, a market, an obligation window
(rolling hours), and a min/max settled notional inside that window:

```toml
[[contracts]]
id = "lp-quarterly-cc-usdc"
counterparty = "party-x-..."
market = "CC-USDC"
window_hours = 24
min_notional = "1000"
max_notional = "100000"
expires_at = "2026-12-31T23:59:59Z"
```

As `SETTLED` events arrive the agent tallies notional per contract and
emits `contract.under_floor` / `contract.over_ceiling` events when limits
are breached. Periodic `contract.status` events report current tallies so
you can see how far you are from each obligation.

## Usage

```bash
agent-contractual-compliance-bilateral --config agent.toml run --contracts bilateral.toml --reload-secs 60 --status-interval-secs 600 --stdout --log-file compliance.log
agent-contractual-compliance-bilateral --config agent.toml run --contracts contracts.toml --webhook https://api.example.com/events --log-file audit.jsonl
agent-contractual-compliance-bilateral --config agent.toml --verbose check --contracts bilateral.toml
```

## Metadata

| Field | Value |
|---|---|
| Category | `compliance` |
| Agent slug | `contractual-compliance` |
| Template | `Bilateral` (rust) |
| Status | `active` |
| Tags | `contracts`, `obligations` |
| Required balance | None — read-only |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-contractual-compliance-bilateral`
- Binary: `agent-contractual-compliance-bilateral`

## Resources

- Live demo: <https://silvana.one/agents/demo/contractual-compliance-bilateral/>
- Contact Us: <https://silvana.one/contact>
