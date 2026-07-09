# Audit Replay — Back-Test

> Back-tests a rule policy against a signed Trading History log. Answer 'how many trades would have been blocked if this new policy had been active since date X?' before rolling it out live.

## What it does

Reads a signed `agent-trading-history` JSONL and replays every `order.created` / `settlement.settled` record through a TOML rule set (same shape as `agent-pre-trade-check` + `agent-compliance-screening` per-market caps). Emits per-event verdicts + a summary with hits-by-rule. Answer "how many trades would have been blocked if this new policy had been active since date X?" before rolling it out live.

## How it works

Audit Replay Agent — offline policy back-test on a trading-history log

Reads an `agent-trading-history` JSONL file line by line and replays
every event through a TOML rule set — the same shape used by
`agent-pre-trade-check`, `agent-compliance-screening`, and
`agent-risk-management`. Emits per-event verdicts (`accept` / `reject`
plus rule hits) so you can answer "how many trades would have been
blocked if this new policy had been active since date X?" before
rolling it out live.

Read-only. Never touches the ledger.

## Usage

```bash
agent-audit-replay-back-test replay --history history.jsonl --rules rules.toml
agent-audit-replay-back-test replay --history history.jsonl --rules rules.toml --emit-accepts --output verdicts.jsonl
agent-audit-replay-back-test check --rules rules.toml
```

## Metadata

| Field | Value |
|---|---|
| Category | `auditing` |
| Agent slug | `audit-replay` |
| Template | `Back-Test` (rust) |
| Status | `active` |
| Tags | `audit`, `policy`, `back-test` |
| Required balance | None — offline |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-audit-replay-back-test`
- Binary: `agent-audit-replay-back-test`

## Resources

- Live demo: <https://silvana.one/agents/demo/audit-replay-back-test/>
- Contact Us: <https://silvana.one/contact>
