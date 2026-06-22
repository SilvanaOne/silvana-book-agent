# batch-order-agent

**Batch Order Management** monorepo on Silvana (`npm` workspaces + Turborepo).

Background and roadmap: `../task/implementation-plan.md`, scope `../task/bam_task.txt`,
phases 0–1 under `../task/docs/`.

## Layout

| Path | Purpose |
|------|---------|
| `apps/api` | HTTP API (Express): portfolio, rebalance preview/execute, audit |
| `apps/worker` | Long-running process: Silvana sanity + heartbeat, BullMQ consumer (full executor — phase 6) |
| `apps/web` | Next.js dashboard |
| `packages/shared` | Domain types and constants |
| `packages/silvana-client` | Wrapper around `@silvana-one/orderbook`, env-driven config |
| `packages/portfolio-engine` | Portfolio math (phase 4) |
| `packages/db` | Prisma client + `@batch-order/db` exports |
| `prisma/` | Schema + dev seed |
| `infra/docker-compose.yml` | PostgreSQL + Redis for local development |
| `infra/silvana-agent/` | Scaffold for the `cloud-agent` onboarding + sidecar (`Dockerfile`, `smoke.sh`). Hybrid with Silvana — see `../task/updated-scenario/hybrid-architecture.md` |
| `infra/cloud-agent.compose.yml` | Compose overlay: passive sidecar (Step 2) |
| `infra/cloud-agent-variant-a.compose.yml` | Compose overlay: long-running `agent --settlement-only` (Step 3, Variant A) |
| `../deploy/` (at monorepo root) | Production deploy: Docker Compose, nginx, **[deploy to bamagent.spcatcher.cfd](../deploy/DEPLOY-BAMAGENT.md)** (`46.250.253.67`), `deploy-to-server.sh` |

## Production (Docker, VPS)

Current server **`46.250.253.67`**, public URL **`https://bamagent.spcatcher.cfd`**
(SSH by key, no password). Full procedure: **`../deploy/DEPLOY-BAMAGENT.md`**.

> Latest snapshot of the production state (what's verified working, what
> Silvana blockers remain, how to test via UI without live Silvana) —
> see **`../task/docs/status-2026-05-22.md`**.

After copying and editing `deploy/.env.stack` (example: `deploy/.env.stack.bamagent.example`):

```bash
export UPLOAD_ENV_STACK=1
./deploy/deploy-to-server.sh --force            # bring up the stack
./deploy/deploy-to-server.sh --force --reseed   # plus rewrite seed targets/positions inside the api container
```

## Quick start

End-to-end local sequence «bring up Postgres/Redis → migrate → seed → build → `turbo dev` (api + web + worker)»:

```bash
cd batch-order-agent
./run.sh --help           # options: --prepare-only, --no-seed, --skip-infra, --with-infra
./run.sh
```

`./run.sh` without flags **auto-detects** whether infra is up: if `postgres`+`redis`
are already running for the compose project `infra/` — `docker compose up` is skipped.
Force skip with `--skip-infra`, force a fresh up with `--with-infra`.

Before `exec npm run dev` the script also **loads the monorepo-root `.env`** via
`set -a; source .env; set +a`, so Next dev sees `NEXT_PUBLIC_*` without a separate
`apps/web/.env`. Key variables:

- `DEMO_TOOLS=1` — server-side gate for `POST /api/portfolio/:id/imitate-drift` and
  (TODO) `rebalance-now`. Without it the endpoint returns 404.
- `NEXT_PUBLIC_DEMO_TOOLS=1` — client-side gate for the **Manual Drift Imitator**
  block on `/rebalance`. Must match `DEMO_TOOLS`.
- `NEXT_PUBLIC_SILVANA_SCAN_URL=https://silvascan.io/devnet/tx/{hash}` — link
  template for the paperclip in the Rebalance modal. Must contain the `{hash}`
  placeholder.

In production these live in `deploy/.env.stack` and are baked into the web image
(see `Dockerfile.web`'s `ARG NEXT_PUBLIC_*` + `docker-compose.yml`'s `build.args`).

Manual setup (equivalent of the first part of `run.sh`):

```bash
cd batch-order-agent
cp .env.example .env # fill in as needed
npm install
npm run build
```

Run individual apps without `turbo dev` on the whole monorepo:

```bash
npm run dev:api     # Express :3000 (see apps/api)
npm run dev:web     # Next :3001 — set NEXT_PUBLIC_API_URL in the root .env if API is not on localhost:3000
npm run dev:worker  # Worker (Silvana pricing sanity at startup)
```

Database (phase 3+):

```bash
docker compose -f infra/docker-compose.yml up -d
# the container exposes Postgres on host port 5433 (to avoid clashing with a local 5432)

# in .env use DATABASE_URL with port :5433 as in .env.example
npm run db:migrate:apply   # prisma migrate deploy + generate

npm run db:seed            # dev portfolio (requires up-to-date Prisma Client — after generate)
```

See `prisma/schema.prisma` and `prisma/migrations/` — full schema + seed.

Silvana smoke test (mirror of `task/scripts/smoke-orderbook`, `.env` lives at the
root of **this** repo):

```bash
cd scripts/smoke-orderbook && npm install && npm run smoke
```

## Hybrid with `cloud-agent` (Rust sidecar)

Background: **`../task/updated-scenario/hybrid-architecture.md`** (TS↔Rust
responsibility map, Variants A and B) and **`../task/updated-scenario/RUNBOOK.md`**
(commands).

Quick hooks once onboarding is done:

```bash
cd infra
# Step 2 (passive sidecar, info party):
docker compose -f docker-compose.yml -f cloud-agent.compose.yml up -d --build cloud-agent

# Step 3 (Variant A — long-running `agent --settlement-only`):
docker compose -f docker-compose.yml -f cloud-agent.compose.yml -f cloud-agent-variant-a.compose.yml up -d --build cloud-agent

# Smoke on top of Step 3 (no real transactions):
./silvana-agent/smoke.sh

# Variant B — TS job `rfq_fill` via CLI bridge:
# in the root .env: WORKER_RFQ_DOCKER_CONTAINER=bam-cloud-agent + WORKER_RFQ_DRY_RUN_DEFAULT=1
```

## Workspace prefix

Scoped workspace: **`@batch-order/*`**.

## Sync to upstream `SilvanaOne/silvana-book-agent`

This monorepo is the **source of truth** for the `batch-order-management` agent.
The published snapshot lives at `SilvanaOne/silvana-book-agent` on the
`new-agents` branch, under `crates/agent-batch-order-management/`.

Dev history lives **here**; upstream receives a single «Sync» commit per
publish. The wrapper script sits at the monorepo root:

```bash
# from the monorepo root (one level above batch-order-agent/)
./scripts/sync-to-silvana.sh "<one-line summary of what changed>"
```

What it does:
1. `git clone --depth 1 --branch new-agents` of the upstream into a temp dir.
2. `rsync --delete --checksum --no-times` `batch-order-agent/` →
   `crates/agent-batch-order-management/`, excluding `node_modules/`, `.next/`,
   `dist/`, `*.tsbuildinfo`, `.env*`, `coverage/`,
   `infra/silvana-agent/runtime*`, `infra/silvana-agent/bin/`.
3. `git commit` with `Sync agent-batch-order-management: <summary>` plus our
   `remote + SHA` for traceability.
4. `git push origin new-agents`.
5. Removes the temp clone.

Dry-run without push: `./scripts/sync-to-silvana.sh --dry-run`.

Override destination:

```bash
UPSTREAM_REPO="git@github.com:SilvanaOne/silvana-book-agent.git" \
UPSTREAM_BRANCH="new-agents" \
UPSTREAM_PATH="crates/agent-batch-order-management" \
./scripts/sync-to-silvana.sh "..."
```

When to run:
- After every `git push` to our origin, if upstream should reflect it.
- Not after every WIP commit — that creates noise in the upstream log. Better
  to batch a few commits and publish under one meaningful «Sync» summary.

There is a symmetric `./scripts/pull-from-silvana.sh` to bring back other
contributors' changes from upstream — see GUIDELINES.md.
