# Readiness Check — Pre-Trade

> Pre-trade gate — verifies required balances, absence of stuck settlements, and presence of TransferPreapproval contracts. Exits non-zero on any failure so deployment scripts can block on it.

## What it does

Verifies the agent is provisioned correctly: required balances, no stuck settlements, presence of TransferPreapproval contracts. Exits non-zero if any check fails — wire it into deployment scripts.

## How it works

Readiness Check Agent — pre-trade gate

Before launching a long-running trading agent, run this check to verify:
- Required balances are present (`--required <instrument>=<amount>` repeatable)
- No stuck pending settlements above a configurable limit
- No `FAILED` settlement proposals (or below a limit)
- At least one `TransferPreapproval` exists (optional via `--require-preapproval`)

Exits with non-zero status if any check fails — wire it into deployment
scripts as a hard gate before starting the production agent.

## Usage

```bash
agent-readiness-check-pre-trade check --required Amulet=50 --required CC-USDC=100
agent-readiness-check-pre-trade check --max-failed-settlements 0 --max-pending-settlements 10 \
                            --require-preapproval --required Amulet=50
```

## Metadata

| Field | Value |
|---|---|
| Category | `tx_flow` |
| Agent slug | `readiness-check` |
| Template | `Pre-Trade` (rust) |
| Status | `active` |
| Tags | `gate`, `pre-trade` |
| Required balance | None — read-only |
| Supported assets | CC, USDC |
| Supported projects | Silvana |

## Package

- Crate: `agent-readiness-check-pre-trade`
- Binary: `agent-readiness-check-pre-trade`

## Resources

- Live demo: <https://silvana.one/agents/demo/readiness-check-pre-trade/>
- Contact Us: <https://silvana.one/contact>
