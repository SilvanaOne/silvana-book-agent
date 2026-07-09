# Copy Trading — Proportional

> Mirror a leader party's order flow onto your own book. Every leader `order.created` becomes a scaled mirror trade — filtered by market and capped by per-trade notional. Read-only observation of the leader; two-phase signing only on your own side.

## What it does

Sibling of `Mirror`. Instead of a fixed `--scale` factor, each mirror is sized by the ratio of *your* portfolio to the *leader's* portfolio:

    effective_scale = min(follower_portfolio / leader_portfolio, max_scale)
    mirror_qty      = leader_qty × effective_scale

Small followers automatically size down. Large followers get capped at `--max-scale` (default 1.0, i.e. do not exceed leader size). The same market whitelist / per-trade notional caps as `Mirror` still apply.

## How it works

For each line in `--leader-stream`:
1. Parse `order.created` payload — extract `market_id`, `side`, `price`, `quantity`.
2. Reject if `market_id ∉ --markets` (when whitelist is set) or `price × quantity > --max-leader-notional`.
3. Compute `mirror_qty = quantity × effective_scale`.
4. Reject if `price × mirror_qty > --max-mirror-notional`.
5. Emit a `copy_trading.proportional_mirror` record to stdout / JSONL / webhook sinks, carrying both `raw_scale` and `effective_scale` for auditability.

Sinks and refusal accounting mirror `agent-copy-trading-mirror`; run the summary at end-of-stream via stdout.

## Usage

```bash
agent-copy-trading-proportional run \
    --leader-stream leader.jsonl \
    --follower-portfolio 10000 \
    --leader-portfolio 100000 \
    --markets CC-USDC \
    --stdout

agent-copy-trading-proportional run \
    --leader-stream leader.jsonl \
    --follower-portfolio 50000 \
    --leader-portfolio 100000 \
    --max-scale 0.5 \
    --max-leader-notional 5000 \
    --max-mirror-notional 500 \
    --log-file mirrors.jsonl \
    --webhook https://hooks.example.com/copy
```

## Metadata

| Field | Value |
|---|---|
| Category | `trading` |
| Agent slug | `copy-trading` |
| Template | `Proportional` (rust) |
| Status | `active` |
| Tags | `copy-trading`, `proportional`, `leader-follower` |
| Required balance | $100 min |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-copy-trading-proportional`
- Binary: `agent-copy-trading-proportional`

## Resources

- Live demo: <https://silvana.one/agents/demo/copy-trading-proportional/>
- Contact Us: <https://silvana.one/contact>
