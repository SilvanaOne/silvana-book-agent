# Trade Explain — Rationale

> Post-hoc rationale + counterfactual for every trade. Reads a trading-history log and emits an audit-friendly explanation of why each order was placed and what would likely have happened if it hadn't been.

## What it does

Trade Explain Agent — post-hoc LLM rationale + counterfactual

## How it works

Trade Explain Agent — post-hoc LLM rationale + counterfactual

Reads a trading-history JSONL and, for each `order.created` or
`settlement.settled`, generates:
  - `rationale`: short natural-language explanation of the trade
  - `counterfactual`: what likely would have happened if the trade
    had NOT been placed
Emits both to a signed explanations JSONL log for auditors.

Deterministic mock text generator (features → template). Replace
`explain_trade` with a real LLM in prod; schema is stable.

## Usage

```bash
agent-trade-explain-rationale explain --history trading-history.jsonl --output explanations.jsonl --include-settlements true --include-orders true --model silvana-mock-explain-v1
agent-trade-explain-rationale explain --history history.jsonl --output explains.jsonl --include-orders true
agent-trade-explain-rationale explain --history trades.jsonl --output output.jsonl --include-settlements false --model custom-explain-v2
```

## Metadata

| Field | Value |
|---|---|
| Category | `intelligence` |
| Agent slug | `trade-explain` |
| Template | `Rationale` (rust) |
| Status | `active` |
| Tags | `ai`, `explain`, `counterfactual`, `audit` |
| Required balance | None — offline |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-trade-explain-rationale`
- Binary: `agent-trade-explain-rationale`

## Resources

- Live demo: <https://silvana.one/agents/demo/trade-explain-rationale/>
- Contact Us: <https://silvana.one/contact>
