# Signal Bot — JSONL

> Tails a JSONL signals file and executes one limit order per line — persists byte offset in a sidecar file so it survives restarts without replay.

## What it does

Tails a JSONL file of trading signals and places one limit order per line. Persists its byte offset in `<signals>.cursor` so it survives restarts without replay.

Signal format:

## How it works

Signal Bot — execute trades from a JSONL signal file

Tails a JSONL file. Each line is one signal:

```json
{"market":"CC-USDC","side":"buy","quantity":"0.5","price":"1.02","ref":"my-strategy-1"}
```

Fields:
- `market` — market id (required)
- `side`   — "buy" | "sell" (required)
- `quantity` — base instrument quantity (required, decimal string)
- `price` — limit price (required, decimal string)
- `ref` — optional trader_order_ref echoed back in the order

The bot persists the byte offset it has read in a sibling file
`<signals>.cursor` so it doesn't replay signals across restarts.

## Usage

```bash
{"market":"CC-USDC","side":"buy","quantity":"0.5","price":"1.02","ref":"my-strategy-1"}
```

```bash

agent-signal-bot-jsonl run --signals-file signals.jsonl
agent-signal-bot-jsonl run --signals-file signals.jsonl --from-end --dry-run
```

## Metadata

| Field | Value |
|---|---|
| Category | `trading` |
| Agent slug | `signal-bot` |
| Template | `JSONL` (rust) |
| Status | `active` |
| Tags | `signals`, `jsonl`, `executor` |
| Required balance | $100 min |
| Supported assets | CC, USDC |
| Supported projects | Silvana |

## Package

- Crate: `agent-signal-bot-jsonl`
- Binary: `agent-signal-bot-jsonl`

## Resources

- Live demo: <https://silvana.one/agents/demo/signal-bot-jsonl/>
- Contact Us: <https://silvana.one/contact>
