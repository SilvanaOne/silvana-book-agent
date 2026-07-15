# Arbitrage Agent

> Cross-venue spread detection across CEX and Canton DEX venues — always-on
> scanner, operator console with kill switch and balances, and optional live
> dual-trade execution routed through Silvana Vault + proprietary engines.

## What it does

The Arbitrage Agent continuously **scans** connected trading venues for price
differences on the same (or equivalent) assets and, when a spread clears the
configured thresholds, **executes a dual trade** — one leg buys on the cheaper
venue, the other leg sells on the more expensive one. Both legs are sized,
fee-checked, and timing-coordinated so the agent does not sit on one-sided
inventory.

The agent ships with a web-based operator console for live spreads, risk
settings, balances, activity history, and a kill switch. In **paper mode** the
full detection + trade pipeline runs without any real orders so behaviour can
be validated before going live.

## Architecture

Arbitrage Agent is a standalone TypeScript monorepo (npm workspaces +
Turborepo) rather than a Rust workspace member. It ships:

- **Scanner** — polls venues, normalizes prices, ranks spreads.
- **Executor** — runs dual-trade legs against CEX + Canton, gated by risk
  limits and the kill switch.
- **Operator console** — Next.js dashboard for spreads, stats, balances,
  and configuration.
- **AI assistant** — advisory hints only; never trades or mutates settings.

Live keys never leave **Silvana Vault** — the agent authenticates as a tenant
via `SIGNER_API_URL` + `SIGNER_API_TOKEN` and receives a scoped runtime bundle
per session.

## Modes

| Mode | Real orders | Vault keys required | Use for |
|---|---|---|---|
| `paper` | No | No | Local trial, threshold tuning, demo |
| `live`  | Yes | Yes | Production dual-trade execution |

## Metadata

| Field | Value |
|---|---|
| Category | `trading` |
| Agent slug | `arbitrage` |
| Template | Arbitrage Agent v1 (typescript) |
| Status | `preview` |
| Tags | `arbitrage`, `cross-venue`, `scanner`, `featured` |
| Required balance | CEX + Canton wallet keys via Silvana Vault |
| Supported assets | CC, USDT, USDCx, CBTC |
| Supported projects | Silvana, Canton |

## Package

- Layout: `apps/`, `packages/`, `prisma/`, `infra/`, `schemas/`, `demo/`
- Not part of the Cargo workspace
- Bootstrap: `./run.sh` (or `npm run dev` for the console alone)
- Console: <http://localhost:3001>

## Resources

- Crate README: [`README.md`](./README.md)
- Operator guidelines: [`GUIDELINES.md`](./GUIDELINES.md)
- Live demo: <https://silvana.one/agents/demo/arbitrage>
- Silvana Vault: <https://vault.silvana.one>
- Contact: <https://silvana.one/contact>
