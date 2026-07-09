# Batch Order Management

> Standalone monorepo agent that provides portfolio, rebalance-preview, execute
> and audit APIs on top of Silvana, backed by PostgreSQL + Redis.

## Overview

Batch Order Management is a TypeScript monorepo (npm workspaces + Turborepo)
rather than a Rust workspace member. It ships its own API, worker, and web
console, plus a production deployment pipeline hosted at
`bamagent.spcatcher.cfd`.

The full architecture, layout, deploy procedure, and Silvana integration story
live in the crate-local `README.md` and `GUIDELINES.md`.

## Metadata

| Field | Value |
|---|---|
| Category | `asset-portfolio` |
| Agent slug | `batch-order-management` |
| Template | TypeScript monorepo |
| Status | `preview` |
| Tags | `batch`, `portfolio`, `rebalance` |
| Required balance | Silvana + Canton wallet keys |
| Supported assets | Silvana-tracked instruments |
| Supported projects | Silvana |

## Package

- Layout: `apps/api`, `apps/worker`, `apps/web`, `packages/*`, `prisma/`, `infra/`
- Not part of the Cargo workspace

## Resources

- Crate README: [`README.md`](./README.md)
- Contributing guidelines: [`GUIDELINES.md`](./GUIDELINES.md)
- Live deploy: <https://bamagent.spcatcher.cfd>
