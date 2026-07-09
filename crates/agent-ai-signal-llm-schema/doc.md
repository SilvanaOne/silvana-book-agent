# AI Signal — LLM Schema

> Wraps an LLM in a strict output schema — natural-language prompt + market context → structured trading signal (market, side, price, quantity, confidence, rationale). Feeds directly into agent-signal-bot.

## What it does

AI Signal Agent — LLM-driven trading-signal generator

## How it works

AI Signal Agent — LLM-driven trading-signal generator

Wraps an LLM in a strict schema: reads a short natural-language prompt
(e.g. "I think CC is going to pump within the hour, medium confidence")
+ a snapshot of the current market context (recent mids + spreads),
calls the model, and emits a structured `{market, side, price, quantity,
confidence, rationale}` signal to a JSONL file that `agent-signal-bot`
consumes.

Deterministic mock model in this binary — swap `mock_llm` for a real
OpenAI / Anthropic client in production; the record schema is stable.
Any signal below `--min-confidence` is dropped.

## Usage

```bash
agent-ai-signal-llm-schema emit --prompt "I think CC is going up, high confidence" --markets CC-USDC,BTC-USD --mids 1.05,62000 --min-confidence 0.55 --signals-file signals.jsonl --stdout
agent-ai-signal-llm-schema batch --prompts-file prompts.txt --markets CC-USDC,BTC-USD --mids 1.05,62000 --signals-file signals.jsonl
agent-ai-signal-llm-schema emit --prompt "bearish on CC short-term" --markets CC-USDC --mids 1.02 --min-confidence 0.50 --stdout
```

## Metadata

| Field | Value |
|---|---|
| Category | `intelligence` |
| Agent slug | `ai-signal` |
| Template | `LLM Schema` (rust) |
| Status | `active` |
| Tags | `ai`, `llm`, `signal-generation` |
| Required balance | None — signal-generation only |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-ai-signal-llm-schema`
- Binary: `agent-ai-signal-llm-schema`

## Resources

- Live demo: <https://silvana.one/agents/demo/ai-signal-llm-schema/>
- Contact Us: <https://silvana.one/contact>
