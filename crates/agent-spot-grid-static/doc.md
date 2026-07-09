# Spot Grid — Static

> Passive market-making grid — stacks bids and offers at fixed levels around the mid and lets price walk through them. No RFQ, no fill loops.

## What it does

Reads `[[markets]]` / `[[markets.bid_levels]]` / `[[markets.offer_levels]]` from `agent.toml`
(same format as `orderbook-cloud-agent`) and runs the shared `OrderManager` grid. No RFQ,
no fill loops — pure passive market making.

## How it works

Spot Grid Agent — minimal grid market-maker

Strict subset of `orderbook-cloud-agent`: places passive bid/offer ladders
around the mid price for every market listed in `agent.toml`, but does NOT
run RFQ handlers or fill loops. Grid management is delegated entirely to the
shared `OrderManager` inside `run_agent`.

The grid is configured via the same `[[markets]]` / `[[markets.bid_levels]]`
/ `[[markets.offer_levels]]` blocks as `orderbook-cloud-agent` (see README).

## Usage

```bash
agent-spot-grid-static run
agent-spot-grid-static run --dry-run                # prepare orders without signing
agent-spot-grid-static status
```

## Metadata

| Field | Value |
|---|---|
| Category | `trading` |
| Agent slug | `spot-grid` |
| Template | `Static` (rust) |
| Status | `active` |
| Tags | `marketmaker`, `grid` |
| Required balance | $500 min |
| Supported assets | CC, USDC |
| Supported projects | Silvana |

## Package

- Crate: `agent-spot-grid-static`
- Binary: `agent-spot-grid-static`

## Resources

- Live demo: <https://silvana.one/agents/demo/spot-grid-static/>
- Contact Us: <https://silvana.one/contact>
