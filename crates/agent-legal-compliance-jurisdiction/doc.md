# Legal Compliance — Jurisdiction

> Jurisdiction-aware rule evaluator — flags orders or settlements that violate the legal obligations of the party's configured jurisdiction.

## What it does

Legal Compliance Agent — jurisdiction rule evaluator

## How it works

Legal Compliance Agent — jurisdiction rule evaluator

Maps parties to jurisdictions, then applies per-jurisdiction rules to live
settlement events. Rules currently supported:

- `allowed_markets` — settlement is only legal when ALL parties' jurisdictions
  list the market as allowed.
- `prohibited_markets` — settlement is illegal if ANY party's jurisdiction
  prohibits the market.
- `max_notional_per_trade` — any party's jurisdiction can cap a single trade.
- `prohibited_counterparty_jurisdictions` — sanction-style block of pair
  combinations (e.g. jurisdiction A cannot trade with jurisdiction B).

Config TOML:

```toml
[party_jurisdictions]
"party-x-..." = "US"
"party-y-..." = "EU"
"party-z-..." = "SG"

[jurisdictions.US]
allowed_markets = ["CC-USDC", "BTC-USD"]
max_notional_per_trade = "1000000"
prohibited_counterparty_jurisdictions = ["IR", "KP"]

[jurisdictions.EU]
prohibited_markets = ["XXX-YYY"]
```

Read-only: emits `legal.violation` events to sinks but does not enforce.

## Usage

```bash
agent-legal-compliance-jurisdiction --config agent.toml run --policy jurisdiction-policy.toml --reload-secs 60 --stdout --log-file legal.jsonl
agent-legal-compliance-jurisdiction --config agent.toml run --policy policy.toml --webhook https://compliance.example.com/events --log-file violations.jsonl
agent-legal-compliance-jurisdiction --config agent.toml --verbose check --policy jurisdiction-policy.toml
```

## Metadata

| Field | Value |
|---|---|
| Category | `compliance` |
| Agent slug | `legal-compliance` |
| Template | `Jurisdiction` (rust) |
| Status | `active` |
| Tags | `legal`, `jurisdiction` |
| Required balance | None — read-only |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-legal-compliance-jurisdiction`
- Binary: `agent-legal-compliance-jurisdiction`

## Resources

- Live demo: <https://silvana.one/agents/demo/legal-compliance-jurisdiction/>
- Contact Us: <https://silvana.one/contact>
