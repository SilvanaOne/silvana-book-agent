# Scam Screening — Threat Feed

> Categorized threat-feed screener — flags any party or instrument that appears in configured scam / abuse categories against the live settlement stream.

## What it does

Scam Screening Agent — categorized threat-feed screener

## How it works

Scam Screening Agent — categorized threat-feed screener

Extension of `agent-blocked-party` with multi-category lists and an
optional HTTP feed source. The agent maintains a set of category buckets;
each bucket has a severity (`info` / `warn` / `critical`) and a list of
party ids. Settlements involving any listed party trigger an alert tagged
with the matched categories.

Categories file (TOML):
```toml
[[category]]
name = "scam_db"
severity = "warn"
source = { kind = "file", path = "scam.list" }

[[category]]
name = "sanctions"
severity = "critical"
source = { kind = "url", url = "https://example.com/sanctions.txt" }

[[category]]
name = "honeypot"
severity = "info"
source = { kind = "inline", parties = ["party-a", "party-b"] }
```

Each list file/URL is a plain-text list, one party id per line, '#' for
comments. Lists refresh every `--refresh-secs`.

## Usage

```bash
agent-scam-screening-threat-feed --config agent.toml run --categories categories.toml --refresh-secs 300 --stdout --log-file scam-alerts.jsonl
agent-scam-screening-threat-feed --config agent.toml run --categories threat-feed.toml --webhook https://security.example.com/alerts --log-file threats.jsonl
agent-scam-screening-threat-feed --config agent.toml --verbose check --categories categories.toml
```

## Metadata

| Field | Value |
|---|---|
| Category | `compliance` |
| Agent slug | `scam-screening` |
| Template | `Threat Feed` (rust) |
| Status | `active` |
| Tags | `scam`, `threat-feed` |
| Required balance | None — read-only |
| Supported assets | CC, USDC, cETH |
| Supported projects | Silvana |

## Package

- Crate: `agent-scam-screening-threat-feed`
- Binary: `agent-scam-screening-threat-feed`

## Resources

- Live demo: <https://silvana.one/agents/demo/scam-screening-threat-feed/>
- Contact Us: <https://silvana.one/contact>
