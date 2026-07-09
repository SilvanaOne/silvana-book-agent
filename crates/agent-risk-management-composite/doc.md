# Risk Management — Composite

> Composite rule engine — checks every strategy action against user-defined position, exposure, and notional limits, and enforces cancellation on breach.

## What it does

Risk Management Agent — composite limits enforcer

## How it works

Risk Management Agent — composite limits enforcer

Combines every check that the more focused risk agents do individually
(open orders count, open notional, pending settlements, failed settlements,
per-market exposure) into one TOML-driven policy. Each cycle evaluates the
whole policy and emits a `risk.status` event with per-check pass/fail
flags. With `--enforce`, breached checks trigger targeted cancellations:

- `max_open_orders` / `max_open_notional` exceeded → cancel the
  newest-`order_id` orders until back inside the limit.
- `per_market_max_notional` exceeded → cancel orders on the offending
  market only.
- `max_pending_settlements` and `max_failed_settlements` are reported but
  *not* enforced (the orderbook owns settlement state — see
  `agent-failure-recovery` and `agent-killswitch` for those flows).

## Usage

```bash
agent-risk-management-composite --config agent.toml run --policy risk-policy.toml --check-interval 30 --enforce --stdout --log-file risk-status.jsonl
agent-risk-management-composite --config agent.toml run --policy policy.toml --webhook https://risk-api.example.com/events --log-file risk.log
agent-risk-management-composite --config agent.toml --verbose check --policy risk-policy.toml
```

## Metadata

| Field | Value |
|---|---|
| Category | `risk` |
| Agent slug | `risk-management` |
| Template | `Composite` (rust) |
| Status | `active` |
| Tags | `risk`, `limits`, `enforcement` |
| Required balance | None — read-only |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-risk-management-composite`
- Binary: `agent-risk-management-composite`

## Resources

- Live demo: <https://silvana.one/agents/demo/risk-management-composite/>
- Contact Us: <https://silvana.one/contact>
