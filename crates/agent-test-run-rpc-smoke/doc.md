# Test Run — RPC Smoke

> Read-only RPC smoke test — verifies connectivity, JWT, and basic queries before launching any ledger-writing agent. Add a market flag to also probe price and depth.

## What it does

Read-only RPC smoke test. Use it before launching ledger-writing agents to verify connectivity, JWT, and basic queries work. (Two-phase prepare/execute dry-runs are already covered by `--dry-run` on each ledger-writing agent.)

## How it works

Test Run Agent — pre-flight healthcheck for the orderbook stack

Read-only validation. Exercises every gRPC service the trading agents
depend on (Orderbook + Pricing via JWT, plus optionally a specific market)
and reports per-check pass/fail. Intended as a quick sanity check before
running a ledger-writing agent (`agent-spot-grid`, `agent-spot-dca`, etc.):
if `agent-test-run-rpc-smoke` fails, the others will too.

NOTE: Two-phase `PrepareTransaction` dry-runs are already covered by the
`--dry-run` flag on each ledger-writing agent (spot-dca, tpsl, cash-buffer,
cloud-agent, etc.). This binary stays focused on the read-only RPCs that
every agent shares.

## Usage

```bash
agent-test-run-rpc-smoke validate
agent-test-run-rpc-smoke validate --market CC-USDC          # adds GetPrice + GetOrderbookDepth checks
```

## Metadata

| Field | Value |
|---|---|
| Category | `tx_flow` |
| Agent slug | `test-run` |
| Template | `RPC Smoke` (rust) |
| Status | `active` |
| Tags | `healthcheck`, `pre-flight` |
| Required balance | None — read-only |
| Supported assets | CC, USDC |
| Supported projects | Silvana |

## Package

- Crate: `agent-test-run-rpc-smoke`
- Binary: `agent-test-run-rpc-smoke`

## Resources

- Live demo: <https://silvana.one/agents/demo/test-run-rpc-smoke/>
- Contact Us: <https://silvana.one/contact>
