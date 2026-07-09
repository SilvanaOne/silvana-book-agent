# Automated TP/SL — Bracket

> Watch a live position and trigger take-profit or stop-loss exits automatically, with optional trailing stops.

## What it does

Bracket / OCO variant. Instead of polling price and firing a *market* exit when a threshold is crossed (as `Classic` does), this variant places **both** the take-profit and the stop-loss as resting limit orders immediately. Whichever side of the book fills first closes the position; the other order is then cancelled and the agent exits.

Advantages:
- No missed exits during network hiccups — both orders live on the book.
- No slippage from a polling cadence — TP/SL trigger at exactly the configured price.
- Symmetric handling of long / short positions.

Trade-offs vs. Classic:
- No trailing stop (that would require re-quoting SL on every peak).
- Both orders lock inventory until one fills.

## How it works

On `run`:
1. Load `--tp` and `--sl` as `Decimal`, round both to the market's tick.
2. Validate: for long, `sl < entry < tp`; for short, `tp < entry < sl`.
3. Determine exit direction from `--side` (long → OFFER, short → BID).
4. Submit TP and SL as separate limit orders via two-phase signing.
5. Poll `GetOrders` every `--poll-secs` seconds.
6. When exactly one of the two order IDs disappears from the active book, cancel the sibling and exit.

## Usage

```bash
agent-tpsl-bracket run --market CC-USDC --side long --quantity 10 --tp 1.5 --sl 1.2

agent-tpsl-bracket run --market CC-USDC --side short --quantity 5 --tp 0.90 --sl 1.10

agent-tpsl-bracket run --market CC-USDC --side long --quantity 10 --tp 1.5 --sl 1.2 \
    --poll-secs 2 --dry-run

agent-tpsl-bracket status
```

## Metadata

| Field | Value |
|---|---|
| Category | `trading` |
| Agent slug | `tpsl` |
| Template | `Bracket` (rust) |
| Status | `active` |
| Tags | `take-profit`, `stop-loss`, `bracket`, `oco` |
| Required balance | $100 min |
| Supported assets | CC, USDC |
| Supported projects | Silvana |

## Package

- Crate: `agent-tpsl-bracket`
- Binary: `agent-tpsl-bracket`

## Resources

- Live demo: <https://silvana.one/agents/demo/tpsl-bracket/>
- Contact Us: <https://silvana.one/contact>
