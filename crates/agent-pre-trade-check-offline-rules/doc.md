# Pre-Trade Check — Offline Rules

> Offline rule engine — validates an order (or a stream of orders) against TOML compliance rules: notional, quantity, price band, market and side allow/blocklists. Exits non-zero on reject.

## What it does

Pure offline rule engine. No network, no JWT — just validate one order (or a stream of orders from stdin) against a TOML rules file. Exits 0 on accept, 2 on reject.

## How it works

Pre-Trade Check Agent — pure rule-engine, no network

Validates one prospective order against a TOML rules file. Designed to be
called from CI / scripts before a real submission:

```bash
agent-pre-trade-check-offline-rules check \
    --market CC-USDC --side buy --quantity 10 --price 1.02 \
    --rules rules.toml && agent-spot-dca run ...
```

Rules file shape (all fields optional):

```toml
max_notional_per_order = "5000"
max_quantity_per_order = "100"
min_price = "0.5"
max_price = "5.0"
blocked_markets = ["XXX-YYY"]
allowed_markets = ["CC-USDC", "BTC-USD"]   # if set, market must be in this list
allowed_sides   = ["buy", "sell"]          # if set, side must match
```

Exits 0 if all rules pass, 2 if any rule rejects, 1 on usage / IO error.
Multi-order mode reads JSON Lines from stdin (one order per line).

## Usage

```bash
agent-pre-trade-check-offline-rules check --market CC-USDC --side buy --quantity 10 --price 1.02 --rules rules.toml
cat orders.jsonl | agent-pre-trade-check-offline-rules check-stdin --rules rules.toml --echo > accepted.jsonl
```

```bash

Rules file (`rules.toml`):
max_notional_per_order = "5000"
max_quantity_per_order = "100"
min_price = "0.5"
max_price = "5.0"
blocked_markets = ["XXX-YYY"]
allowed_markets = ["CC-USDC", "BTC-USD"]
allowed_sides   = ["buy", "sell"]
```

## Metadata

| Field | Value |
|---|---|
| Category | `compliance` |
| Agent slug | `pre-trade-check` |
| Template | `Offline Rules` (rust) |
| Status | `active` |
| Tags | `rules`, `offline`, `validator` |
| Required balance | None — offline |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-pre-trade-check-offline-rules`
- Binary: `agent-pre-trade-check-offline-rules`

## Resources

- Live demo: <https://silvana.one/agents/demo/pre-trade-check-offline-rules/>
- Contact Us: <https://silvana.one/contact>
