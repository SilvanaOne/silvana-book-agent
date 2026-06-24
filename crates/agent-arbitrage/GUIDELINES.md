# Arbitrage Agent — guidelines

Short, opinionated guide for operators. Read this first if you are deploying your
own instance; for a one-paragraph overview of what the agent does, see
`README.md`.

---

## What the agent does (one paragraph)

The Arbitrage Agent continuously **scans** connected trading venues for price
differences on the same (or equivalent) assets, then **executes dual trades** when
an opportunity clears your thresholds — buying where the price is lower and
selling where it is higher. An operator console shows live spreads, configuration,
balances, and activity. In **paper mode** the full loop runs without real orders so
you can validate behaviour before going live. When you are ready for live trading,
register your API keys and wallet material in **Silvana Vault** and point the agent
at your tenant identity.

---

## Prerequisites

- A Linux server (or your laptop for a local trial) with **Docker** and the
  **Docker Compose v2** plugin.
- **Git** to clone and update the source.
- For live balances and trading: a **Silvana Vault** account at
  [vault.silvana.one](https://vault.silvana.one) with your exchange API keys and
  Canton wallet loaded.
- A **platform API token** (`SIGNER_API_TOKEN`) issued when you order or host the
  agent through Silvana — needed so the agent can talk to Vault securely.

---

## Get the source code

Published open-core source lives in the Silvana agents monorepo:

```bash
git clone --branch new-agents \
  https://github.com/SilvanaOne/silvana-book-agent.git
cd silvana-book-agent/crates/agent-arbitrage
```

To update an existing checkout:

```bash
cd silvana-book-agent
git pull origin new-agents
cd crates/agent-arbitrage
```

You can clone on your **workstation** and rsync to a server, or clone directly on
the server — either way, all commands below assume your shell is in
`crates/agent-arbitrage` (the agent root, next to `README.md` and `run.sh`).

---

## Deploy on your server

### 1. Configure environment

Copy the stack template and edit it with your domain, secrets, and Vault binding:

```bash
cp infra/deploy/.env.stack.arbitrage.example infra/deploy/.env.stack
```

Important variables in `infra/deploy/.env.stack`:

| Variable | Purpose |
|---|---|
| `PUBLIC_SITE_URL` | URL where operators open the dashboard. Use your HTTPS origin in production, or `http://localhost:3001` when running locally (including Docker on your own machine). |
| `NEXT_PUBLIC_API_URL` | Same origin the browser uses to reach the API (usually identical to `PUBLIC_SITE_URL`; for local use `http://localhost:3001` or `http://localhost:3000` depending on how you proxy). |
| `POSTGRES_PASSWORD` | Database password — change from the default before production. |
| `TOKEN_SECRET` | HMAC secret for operator sessions when login is enabled (`openssl rand -hex 32`). |
| `STRATEGY_MODE` | `paper` (simulated trades) or `live` (real execution when keys and Silvana onboarding are ready). |
| `SIGNER_API_URL` | `https://vault.silvana.one` |
| `SIGNER_API_TOKEN` | Platform token shared with Vault (from Silvana). |
| `SIGNER_TENANT_USER_ID` | Your **Agent User ID** from Vault — see next section. |
| `DISABLE_AUTH` / `NEXT_PUBLIC_DISABLE_AUTH` | Set both to `1` to skip login during setup; set to `0` and fill `OPERATOR_USERNAME` / `OPERATOR_PASSWORD_BOOTSTRAP` before exposing the UI publicly. |

Never commit `infra/deploy/.env.stack` — it holds secrets.

### 2. Build and start containers

From the agent root:

```bash
docker compose --env-file infra/deploy/.env.stack \
  --project-directory infra/deploy \
  -f infra/deploy/docker-compose.yml up -d --build
```

This starts Postgres, Redis, the API, the scanner, the web dashboard, and an
nginx gateway. Check status:

```bash
docker compose --project-name arbitrage \
  --env-file infra/deploy/.env.stack \
  -f infra/deploy/docker-compose.yml ps
```

### 3. Run database migrations (first deploy and after schema updates)

```bash
docker compose --project-name arbitrage \
  --env-file infra/deploy/.env.stack \
  -f infra/deploy/docker-compose.yml exec api \
  npx prisma migrate deploy --schema prisma/schema.prisma
```

### 4. Put TLS in front (production)

Point your domain at the server and terminate HTTPS with your preferred reverse
proxy (nginx, Caddy, Traefik, etc.), forwarding to the gateway ports published by
the stack. Set `PUBLIC_SITE_URL` and `NEXT_PUBLIC_API_URL` to that HTTPS origin
before rebuilding the `web` service so the browser calls the correct API.

### 5. Routine updates

After `git pull`, rebuild and restart:

```bash
docker compose --env-file infra/deploy/.env.stack \
  --project-directory infra/deploy \
  -f infra/deploy/docker-compose.yml up -d --build
```

Postgres data persists in the Docker volume `pgdata_arbitrage_deploy` across
restarts and redeploys on the same host.

---

## Register keys in Silvana Vault

Vault is the only place your private keys live. The agent never stores plaintext
exchange or wallet secrets on disk.

1. Open [vault.silvana.one](https://vault.silvana.one) and go to **Register**
   (`/register`). Create your account and sign in.
2. On the Vault home page, copy your **Agent User ID** — a UUID such as
   `80962ef5-3b4b-44e1-91bb-560181675ddd`. You will paste this into the agent
   config as `SIGNER_TENANT_USER_ID`.
3. Open the **Load keys** tab in Vault and add the credentials your agent needs:
   - **Bybit** and **KuCoin** API keys (read + trade permissions as required by
     your strategy).
   - **Canton** wallet — either an Ed25519 private key or a BIP39 seed phrase.
     Keys are encrypted in your browser (RSA-OAEP) before upload; Vault stores
     ciphertext only.
4. Optionally open **Balances** in Vault to confirm Vault can read your CEX
   balances with the keys you uploaded.
5. In `infra/deploy/.env.stack` on your agent server, set:

   ```env
   SIGNER_API_URL=https://vault.silvana.one
   SIGNER_API_TOKEN=<platform token from Silvana>
   SIGNER_TENANT_USER_ID=<your Agent User ID>
   ```

6. Restart the agent stack so the API picks up the new values:

   ```bash
   docker compose --project-name arbitrage \
     --env-file infra/deploy/.env.stack \
     -f infra/deploy/docker-compose.yml up -d
   ```

7. When Silvana onboarding for your party is complete, switch `STRATEGY_MODE=live`
   and restart. Until then, keep `STRATEGY_MODE=paper` — the scanner and console
   work fully; trades are simulated.

The agent **Balances** tab (see below) mirrors holdings Vault knows about for your
tenant, refreshed on a background interval.

---

## Run locally (workstation)

For development or a quick trial without a remote server:

```bash
./run.sh
```

Then open **http://localhost:3001**. If you use the Docker stack locally instead,
set `PUBLIC_SITE_URL=http://localhost:3001` and `NEXT_PUBLIC_API_URL=http://localhost:3001`
in `infra/deploy/.env.stack` (or the port your gateway publishes).

Postgres and Redis start via Docker; `run.sh` runs migrations, builds workspaces, and launches the API, scanner, and
web UI in the background.

Useful subcommands:

```bash
./run.sh status    # is the agent running?
./run.sh logs      # tail application logs
./run.sh stop      # stop background processes
./run.sh --help    # prepare-only, skip-infra, foreground mode
```

Local defaults use **paper mode** and **auth disabled** so the dashboard opens
immediately.

---

## Start the agent and monitor it

### Production (server)

1. Complete Vault key registration and `.env.stack` as above.
2. Ensure containers are up (`docker compose … ps`).
3. Open your public URL (`PUBLIC_SITE_URL`) in a browser.
4. Watch container logs if something looks wrong:

   ```bash
   docker logs -f arbitrage-scanner
   docker logs -f arbitrage-api
   docker logs -f arbitrage-web
   ```

### What to watch in the UI

- **Dashboard** — live spread stream (green *live* dot when SSE is connected),
  kill-switch state, and recent opportunities.
- **Config → Start / Stop** — pauses or resumes the scanner without redeploying.
- **Balances** — confirms Vault keys are wired and balances are syncing.
- **Stats** — longer-horizon view of where opportunities cluster.

A typical first session:

1. Deploy with `STRATEGY_MODE=paper`.
2. Open **Dashboard** — confirm venues show as connected and spreads appear in the
   table within a minute.
3. Tune **Config** — minimum spread, trade size, risk limits.
4. Register keys in Vault; verify **Balances**.
5. When ready for live trading, set `STRATEGY_MODE=live`, restart, and keep the
   kill switch handy on **Dashboard** or **Config**.

---

## UI tabs

The web dashboard lives at `/` on the web port (local: **3001**; production: your
`PUBLIC_SITE_URL`). Primary navigation is the top tab bar.

### Dashboard (`/`)

The main operator surface.

- **Kill switch** — master pause. When engaged, the scanner stops detecting new
  opportunities; release to resume.
- **Runtime / Trade config / Risk limits** — quick view and inline edit of
  strategy mode, scan interval, minimum spread, trade size, quote-age limits, daily
  loss cap, and consecutive-loss guardrails.
- **Venues** — one card per connected venue with logo and health
  (connected / stale / offline).
- **Spread stats & Paper P&L** — rolling counters and simulated profit/loss while
  in paper mode.
- **Live spread chart** — time series of recent spread sizes (SSE-backed).
- **Recent spreads** — sortable table of detected opportunities; cross-cluster
  rows are visually tagged.
- **Trade log, Inventory, Audit** — chronological feeds of simulated or live
  activity, per-venue inventory snapshots, and configuration changes.

### Stats (`/stats`)

Profitability analytics over accumulated spread history:

- **Spreads by venue** — horizontal bar chart of opportunity count per venue.
- **Share by venue** — donut chart of where opportunities originate.
- **Spread-size distribution** — histogram of spread magnitudes with hover
  tooltips.

Charts refresh automatically every few seconds. Until enough live data exists, a
curated demo set keeps the page readable.

### Config (`/config`)

Full-screen control panel for parameters that do not require a redeploy:

- **Start / Stop** — large runtime toggle tied to the same kill switch as the
  dashboard.
- **Trade parameters** — target spread, trade size in USD, enabled token pairs.
- **Risk parameters** — max quote age, daily loss limit, max consecutive losses.
- **Venue grid** — per-venue enable flags and health, with tooltips explaining
  each field.

Changes persist to Postgres and appear in the audit log; the running scanner picks
them up on its next cycle.

### Balances (`/balances`)

Read-only view of holdings tied to the keys you loaded in Vault:

- Total portfolio value in USD and last refresh timestamp.
- One card per key source (CEX account or Canton party) with per-asset free,
  locked, and USD columns.

If the page is empty, finish Vault registration and confirm
`SIGNER_TENANT_USER_ID` matches your Vault **Agent User ID**.

### AI Assistant (`/assistant`)

Advisory-only chat. The assistant reads the same live spreads, config, and stats
feeds as the dashboard and **suggests** actions — it never places trades or
mutates configuration. Use preset prompts or type your own questions; any
actionable suggestion still requires you to confirm manually in the console.

### AI Manager (`/manager`)

Preview of future **delegated** automation — toggles for capabilities such as
auto-tuning spread targets or pausing on anomalies. Today these delegations are
stored in browser local storage only and do not execute; the hot trading path
remains deterministic. Grant or revoke capabilities here to rehearse how
supervised automation will work in a later release.

### Login (`/login`)

Shown when operator authentication is enabled (`DISABLE_AUTH=0`). Bootstrap
credentials come from `OPERATOR_USERNAME` and `OPERATOR_PASSWORD_BOOTSTRAP` in
`.env.stack` on first startup; rotate via the API once logged in.

---

## Related docs

- `README.md` — high-level description of scanning and dual-trade behaviour.
- [Silvana Vault](https://vault.silvana.one) — register, load keys, copy Agent User ID.
- [Agent source on GitHub](https://github.com/SilvanaOne/silvana-book-agent/tree/new-agents/crates/agent-arbitrage) — open-core tree in the `new-agents` branch.
