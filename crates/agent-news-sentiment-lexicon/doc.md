# News Sentiment — Lexicon

> Consumes a stream of news headlines, scores sentiment on a lexicon, and emits a market-tagged signal for headlines whose |sentiment| clears the threshold AND that mention a configured market alias.

## What it does

News Sentiment Agent

## How it works

News Sentiment Agent

Consumes a stream of news headlines (from a JSONL file — one headline
per line, `{ ts, headline, source? }`), scores each headline in the
range `[-1..+1]` on a simple keyword lexicon, extracts market mentions
from a `market → aliases` map, and emits a signal for headlines whose
`|sentiment| > threshold` AND whose text mentions at least one
configured market.

The lexicon is deliberately basic (positive/negative word lists) so
that this binary is dependency-free and deterministic. Swap in a real
model (Vader, RoBERTa fine-tune) in production behind the same schema.

## Usage

```bash
agent-news-sentiment-lexicon scan --news-file headlines.jsonl --threshold 0.35 --market-alias "CC-USDC=canton,cc" --market-alias "BTC-USD=bitcoin,btc" --signals-file signals.jsonl --stdout
agent-news-sentiment-lexicon scan --news-file news.jsonl --threshold 0.50 --market-alias "CC-USDC=cc" --market-alias "BTC-USD=btc" --quantity 2.0 --signals-file out.jsonl
agent-news-sentiment-lexicon scan --news-file feed.jsonl --threshold 0.25 --market-alias "CBTC-CC=cbtc" --stdout --quantity 0.5
```

## Metadata

| Field | Value |
|---|---|
| Category | `intelligence` |
| Agent slug | `news-sentiment` |
| Template | `Lexicon` (rust) |
| Status | `active` |
| Tags | `news`, `sentiment`, `nlp` |
| Required balance | None — signal-generation only |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-news-sentiment-lexicon`
- Binary: `agent-news-sentiment-lexicon`

## Resources

- Live demo: <https://silvana.one/agents/demo/news-sentiment-lexicon/>
- Contact Us: <https://silvana.one/contact>
