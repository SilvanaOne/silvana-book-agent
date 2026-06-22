# Batch Order Management Agent — guidelines

Short, opinionated guide for operators and contributors. Read this first if
you're new to the agent; for code structure, see `README.md`.

---

## What the agent does (one paragraph)

The Batch Order Management Agent is an **operator console for managing a
portfolio of crypto assets through Silvana's private orderbook**. The
operator declares target weights (e.g. WBTC 25%, WETH 25%, CC 10%, USDC 40%);
the agent reads live position snapshots, computes how far the portfolio has
drifted from those targets, and proposes (or executes) a batch of orders that
brings the portfolio back to balance. Heavy lifting — order planning, risk
checks, queue, audit trail — happens in the agent itself; settlement on-chain
is delegated to Silvana's `cloud-agent` sidecar in production. The agent has
**no LLM**. It is plain server software: Postgres for state, Redis for the
job queue, Express for the API, Next.js for the dashboard.

---

## Architecture (one paragraph)

Four pieces work together. **`apps/web`** (Next.js, port 3001) renders the
dashboard and proxies HTTP calls to the API. **`apps/api`** (Express, port
3000) is the only thing the web ever talks to; it owns portfolio CRUD,
rebalance preview/execute, audit. **`apps/worker`** (long-running Node
process) consumes the BullMQ `batch-order-commands` queue, runs the full
rebalance pipeline (router → risk → batch submit) and, when configured,
streams order events from Silvana. **Postgres** holds portfolios, targets,
position snapshots, rebalance jobs, order batches, orders, and audit logs.
**Redis** is the job queue. In production, a Rust **`cloud-agent` sidecar**
sits next to the worker and handles two-phase signing, RFQ taker, and DvP
settlement — what TypeScript can't do on its own. See
`../task/updated-scenario/hybrid-architecture.md` for the TS↔Rust responsibility
split.

---

## Modes of operation

The agent runs in one of three modes, depending on environment:

- **`plan_only`** (default when `SILVANA_JWT` is empty): the worker computes
  plans but never sends orders to the book. Drift Imitator and Rebalance Now
  (when `DEMO_TOOLS=1`) **simulate** results by rewriting `PositionSnapshot`
  directly in Postgres. Transaction hashes the UI shows are locally generated
  via `randomBytes(32)`; they do not exist on any block explorer. This is
  the mode the current `bamagent.spcatcher.cfd` deployment is in.
- **`rpc`** (`SILVANA_JWT` set): the worker uses `@silvana-one/orderbook` to
  send `submitOrder` / `cancelOrder` directly to the orderbook. Fills come
  back through the stream consumer and update `PositionSnapshot` naturally.
- **`hybrid` with `cloud-agent`** (Variant A or B from
  `hybrid-architecture.md`): the Rust sidecar handles settlement and/or RFQ;
  the TS worker either coexists or delegates job-by-job. This is the
  production target once Silvana onboarding clears.

The current external blocker is on Silvana's side — `prepaid_traffic /
credit_limit` for our party hasn't been seeded yet. See
`../task/request-to-silvana/onboarding-credit-limit-for-demo-2026-05-22.md`.

---

## UI tabs

The web dashboard is at `http://localhost:3001` locally and
`https://bamagent.spcatcher.cfd` in production. Each tab below maps to one
page under `apps/web/app/`.

### Portfolio (`/portfolio`)

The home of the operator. Shows everything about the active portfolio:

- **Header line.** Portfolio UUID and a link to the seed UUID. To the right:
  *Portfolio value* (the sum of position market values in the quote
  currency, e.g. USDC) and *drift (max abs)* — the worst-case current
  deviation across all enabled targets, expressed as a weight delta (0.01 =
  1% of portfolio value).
- **Auto-refresh indicator.** A short note under the ID line: «Auto-refresh:
  every 5s · last update Ns ago». The page polls
  `/api/backend/portfolio/:id` in the background, so any change made
  elsewhere (Drift Imitator, Rebalance Now, manual SQL) shows up within
  ~5 seconds without F5. After a `router.refresh()` (e.g. after saving
  edited targets) the page re-syncs instantly via `useEffect`.
- **Targets section.** Table of enabled targets per asset: asset symbol,
  weight (fraction in [0, 1]), min, max, on/off. Above the table sits an
  **Edit** button — opens an inline form with one row per asset, a live
  «sum of enabled weights» indicator (must equal 1.0 ± 0.001), plus
  «+ Add asset» / Save / Cancel. Below the table sits a **Rebalance** button
  — see Rebalance modal next.
- **Rebalance modal.** Clicking *Rebalance* opens a modal dialog. It calls
  `POST /api/portfolio/:id/rebalance-now`, shows a spinner while the request
  runs, then renders a Transfers table: from-asset → to-asset, amount sent,
  amount received, and a paperclip 📎 link to the Silvana block explorer
  for each transfer. In `plan_only` mode a banner warns that hashes are
  locally generated and will not resolve on the explorer. OK closes the
  modal and triggers `router.refresh()`, so donut/bars/Drift jump to the new
  state.
- **Drift section.** Asset, target, actual, Δ weight (signed), «below
  threshold» (true if `|Δ| < rebalance threshold` from the API env).
- **Snapshots section.** Latest known positions: asset, qty, price,
  position value.
- **Charts.** A donut chart («Current mix») showing share of portfolio value
  per asset, and a bar chart («Target vs actual») with two bars per asset.
  The Target bars share a single salad-green colour; Actual bars use the
  asset's own palette colour from the donut. Both legend swatches re-use
  the same colour as their bars; legend text is always dark, regardless of
  swatch.

### Rebalance (`/rebalance`)

A power-user surface for triggering rebalance work outside the one-click
`/portfolio` modal. Three operations live here:

- **Simulate (preview).** Calls `POST /api/rebalance/preview`. Persists a
  `RebalanceJob` row with `mode=preview` so it shows up in `/monitor`, but
  does not enqueue any worker job. Returns the drift table, planned orders,
  estimated notional, and risk-gate verdict.
- **Dry run execution.** Same pipeline as Live, but with `dryRun=true`.
  Creates a `RebalanceJob` in the queue, the worker picks it up and goes
  through router + risk + batch submit *without* sending to any venue.
  Useful for end-to-end testing of the queue and worker without spending
  any real (or even testnet) value. Finishes with `phase: dry_run_completed`
  in the job output.
- **Live execute.** Same as Dry run, but `dryRun=false`. The button is
  disabled until the latest Simulate confirms all risk gates pass. In
  `plan_only` mode this still produces a queued job, but the worker
  immediately skips the submit step.

Form inputs:

- **Portfolio ID.** Defaults to the seed UUID.
- **Threshold (weight).** Drift filter as a decimal share (0.01 = 1%).
  Assets whose `|Δ weight|` is below the threshold are skipped in the plan.
  Empty = use the API default from `REBALANCE_THRESHOLD_BPS`.
- **Targets JSON.** A what-if override for this single computation only.
  Leave as `[]` to use the portfolio's saved targets from Postgres. Useful
  for sensitivity checks; never written back to the DB.

At the bottom (visible only when `NEXT_PUBLIC_DEMO_TOOLS=1`) sits the
**Manual Drift Imitator** — a testing tool that pretends the market moved.
Pick one of your enabled assets, enter a weight delta (e.g. `0.15` to
overshoot the target by 15% of portfolio value, or `-0.10` to undershoot
by 10%), click Apply. The chosen asset's share of the portfolio moves by
that much; the rest of the assets absorb the change pro-rata to their own
target weights, so the ratios between counter-assets stay the same. Total
portfolio value and individual asset prices stay the same — only the
proportions change. After Apply, the donut and bars on `/portfolio` will
reflect the new mix within one poll tick.

### Monitor (`/monitor` and `/monitor/[jobId]`)

Job-level visibility. The list page enumerates recent `RebalanceJob` rows
with status, mode, timestamps. The detail page is the only page in the app
that **client-side polls** the server (every ~2.5 s) until the job reaches
`completed` or `failed`. It renders:

- **Meta.** Job ID, mode, requested by, created/started/finished timestamps.
- **Input.** The frozen snapshot of inputs (targets, positions, threshold)
  the worker saw when it started — for reproducibility.
- **Output.** Final phase (e.g. `dry_run_completed`), execution mode,
  portfolio-engine result (nav, drift table, planned orders, normalised
  targets), warnings.
- **Batches.** If the worker actually built an `OrderBatch`, each batch is
  listed with its venue, market, counts, and the individual orders. In
  `plan_only + dry_run` this section is intentionally empty — the worker
  does not build batches without a real venue.
- **Settlements.** Settlement events for that job (Variant A/B only).

### Venues (`/venues`)

Read-only inventory of the venue adapters the agent knows about. Each card
shows: venue id, type (`book` / `rfq` / virtual), enabled state, the
markets it claims to support. In `plan_only` the only "venue" you'll
actually see is the Silvana virtual one. Below the cards sits a small
preview-route widget: pick a market and a side, click Preview, and the
execution router shows you which venue it would pick and why
(`POST /api/execution/preview-route`).

### Audit (`/audit`)

Append-only log of meaningful actions. Each row is a `(action, entityType,
entityId, actorId, payload, createdAt)` tuple. Use it to trace cause and
effect after a session:

- `portfolio.targets.updated` — Edit Targets save (before/after in payload).
- `portfolio.drift.imitated` — Drift Imitator Apply (asset, delta, new weights).
- `portfolio.rebalance.now` — Rebalance modal action (transfers count, mode).
- `rebalance.preview` — Simulate call.
- `rebalance.execute.enqueued` — Dry run / Live execute queued.
- `rebalance.execute.risk_blocked` — Live execute aborted by risk gate.
- `rfq.fill.*` — Variant B (job `rfq_fill`) lifecycle.

The endpoint is `GET /api/audit/entries?limit=N` (the underlying table is
`AuditLog`; the route is named `entries` rather than `logs` to avoid the
upstream `silvana-book-agent` `.gitignore`'s `logs/` rule).

---

## Operator workflow

For a typical demo cycle (in `plan_only`):

1. Open **`/portfolio`**.
2. *Edit* targets → save. Donut and bars update on next poll tick.
3. Go to **`/rebalance`** → Manual Drift Imitator at the bottom → pick an
   asset, enter a weight delta (e.g. `0.10`) → Apply.
4. Back on **`/portfolio`** the donut/bars will have shifted off-target.
5. Click *Rebalance* under the Targets table → spinner → Transfers report
   → OK. Donut/bars return to the targets.
6. Check **`/audit`** to see the trail of `portfolio.drift.imitated` and
   `portfolio.rebalance.now` events.

For live trading, the same flow applies, but `/rebalance` → *Live execute*
replaces the Rebalance modal, and steps 2–4 are driven by real market
movements rather than the imitator.

---

## Database persistence and backups

Postgres state — including the **Audit log**, every `RebalanceJob`,
`OrderBatch`, and the user-edited `PortfolioTarget` rows — lives in a
Docker named volume (`pgdata_batch_order_deploy`) on the VPS. The volume
is preserved across `docker compose up --force-recreate`, so on the
**same server** the audit log keeps accumulating across deploys; it is
only at risk if you migrate to a **different server** or the disk dies.

The deploy script ships with two safety nets:

* **Auto-backup before recreate.** Every
  `./deploy/deploy-to-server.sh --force` runs
  `pg_dump --clean --if-exists -U postgres batch_agent | gzip` against
  the running container and stores the result under
  `${REMOTE_WORK}/backups/auto-<UTC>.sql.gz` before touching anything.
  The last 5 dumps are kept; older ones are pruned. On a fresh server
  (postgres not running yet) the step is silently skipped.
* **Off-server pull.** `./scripts/backup-postgres.sh [output.sql.gz]`
  streams the same `pg_dump --clean --if-exists` from the VPS to your
  local `./backups/bam-postgres-<UTC>.sql.gz` (gitignored). Run it
  whenever you want a portable copy on your workstation or right before
  a planned migration.

To move the database to another server:

```bash
# 1. on your laptop, with the current VPS still alive
./scripts/backup-postgres.sh
# → ./backups/bam-postgres-…sql.gz

# 2. point the deploy at the new host and pass the dump as restore input
DEPLOY_SERVER=root@<new-host> ./deploy/deploy-to-server.sh \
  --force --restore-from=./backups/bam-postgres-<…>.sql.gz
```

`--restore-from=` rsyncs the dump to
`${REMOTE_WORK}/backups/restore-input.sql.gz`, waits for postgres to
accept connections after `docker compose up`, then loads it through
`psql -v ON_ERROR_STOP=1`. The dump's own `DROP IF EXISTS … CREATE …`
statements make the restore idempotent against either a freshly seeded
or already-populated target database. After a successful load the input
file is removed so the next routine deploy does not re-apply it.

`--restore-from` and `--reseed` are mutually exclusive — a restore brings
its own data and would clash with the seed script's `deleteMany +
createMany`.

For protection against hardware loss (the VPS disk dies and takes the
backups with it) you'd want off-server storage (S3 / Backblaze) or a
managed Postgres (Neon / Supabase). Not wired today; would be a separate
evolution.

---

## Related docs

- `README.md` — how to develop the agent locally and
  publish to upstream.
- `../deploy/DEPLOY-BAMAGENT.md` — production deploy procedure.
- `../task/docs/short-description.md` — what the agent is in plain language.
- `../task/docs/status-2026-05-22.md` — current snapshot of what's deployed,
  what's queued, what's blocked.
- `../task/updated-scenario/RUNBOOK.md` — onboarding + sidecar commands.
- `../task/updated-scenario/hybrid-architecture.md` — TS↔Rust split.
- `../task/request-to-silvana/` — open escalations to Silvana managers.
