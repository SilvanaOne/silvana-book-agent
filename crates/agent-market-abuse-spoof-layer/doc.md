# Market Abuse — Spoof/Layer

> Self-audit against your own order flow — flags spoofing bursts and layering patterns on your own party before regulators or counterparties notice.

## What it does

Streams the agent's own order events. Flags **spoofing** (a burst of fast cancels: `--spoof-burst` cancels within `--spoof-burst-window-secs` where each cancel landed ≤ `--spoof-window-secs` after the create) and **layering** (`--layer-min-orders` open same-side orders on one market clustered within `--layer-price-band-pct`). Visibility is limited to own flow by JWT scope — this is a self-audit / bug-catcher.

## How it works

Market Abuse Screening Agent

Watches this party's own order stream and flags two classic abuse patterns:

- **Spoofing**: an order is cancelled within `--spoof-window-secs` of being
  created. A handful of fast cancels is normal market making, but a sustained
  burst beyond `--spoof-burst` within `--spoof-burst-window-secs` is the
  pattern auditors look for.
- **Layering**: at least `--layer-min-orders` open orders on the same side
  of the same market clustered within `--layer-price-band-pct` of one
  another. This is the classic "stacked book" pattern.

Both checks run on **own orders** (the only ones a JWT-authenticated
subscription exposes), so the agent is suited to compliance self-audit and
to catching strategy bugs that accidentally produce abuse-shaped flow,
rather than detection of third parties. Alerts go to stdout / JSONL file /
HTTP webhook.

## Usage

```bash
agent-market-abuse-spoof-layer run --spoof-burst 10 --layer-min-orders 5 --layer-price-band-pct 1.0 --stdout
agent-market-abuse-spoof-layer run --market CC-USDC --webhook https://hooks.example.com/abuse --log-file abuse.jsonl
```

## Metadata

| Field | Value |
|---|---|
| Category | `compliance` |
| Agent slug | `market-abuse` |
| Template | `Spoof/Layer` (rust) |
| Status | `active` |
| Tags | `spoofing`, `layering`, `self-audit` |
| Required balance | None — read-only |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-market-abuse-spoof-layer`
- Binary: `agent-market-abuse-spoof-layer`

## Resources

- Live demo: <https://silvana.one/agents/demo/market-abuse-spoof-layer/>
- Contact Us: <https://silvana.one/contact>
