# Deploying demos via Docker

Every agent demo ships with its own `Dockerfile` in
`crates/agent-<name>/demo/`. A root-level `docker-compose.demos.yml`
orchestrates all of them.

**Two deployment modes:**
- **Prod** — GitHub Actions builds + pushes images to GHCR on every push;
  the VPS pulls them and rolls containers. No source code / node deps
  needed on the host.
- **Local dev** — build from source, run `docker compose ... up -d --build`.

## GitHub Actions (already configured)

`.github/workflows/demos.yml` builds every demo on push to `new-agents`
or `main` and publishes to `ghcr.io/<owner>/<name>-demo:<tag>`. Tags:

- `latest` — most recent push to `new-agents`
- `sha-<short>` — every commit
- `<branch>` — branch name
- `pr-<n>` — pull-request builds (validation only, not pushed)

**First-time GHCR visibility:** by default images are private. Make them
public: GitHub → repo Settings → Packages → each package → change to
Public. Or leave private and use `docker login ghcr.io` on the server.

### Auto-discovery (zero hand-maintained lists)

Demos are **never listed by hand**. `scripts/discover-demos.sh` finds every
`crates/*/demo/` folder that has a `Dockerfile`, derives its image name
(`<agent-without-agent-prefix>-demo`) and reads its port from the Dockerfile
(`ENV PORT=` / `EXPOSE`). This one source of truth drives:

- the **build matrix** (dynamic, via `fromJSON`), and
- the **compose file** (`scripts/gen-demos-compose.sh` regenerates it at deploy
  time from all discovered demos).

Drop in a new `crates/agent-<name>/demo/` with a Dockerfile and it is built,
published and deployed automatically — no edits to the workflow or compose file.

### Selective build & caching

Two layers keep builds cheap:

1. **Per-demo change detection** (`discover` job): a `git diff` over exactly the
   commits this push introduced (`github.event.before..github.sha`; the PR range
   for PRs) selects which discovered demos changed. Only those are rebuilt+pushed;
   unchanged demos are skipped and keep their images. `workflow_dispatch` (and the
   first push of a fresh branch) forces all.
2. **Layer cache** (`type=gha`): even when a demo does rebuild, unchanged Docker
   layers are restored from the GitHub Actions cache, so a rebuild is fast.

## Server setup (once per host)

Install Docker + compose plugin:

```bash
curl -fsSL https://get.docker.com | sh
sudo usermod -aG docker $USER      # log out + back in
```

Clone the repo (only for the compose file + Caddy config; no build):

```bash
sudo mkdir -p /opt && cd /opt
git clone https://github.com/SilvanaOne/silvana-book-agent.git
cd silvana-book-agent
```

If images are private:

```bash
echo "<gh_pat_with_read:packages>" | docker login ghcr.io -u <user> --password-stdin
```

## Bring the demos up

```bash
bash scripts/deploy-demos.sh          # latest
bash scripts/deploy-demos.sh sha-abc  # pin a specific commit
```

At its core the script runs:

```
docker compose -f docker-compose.demos.yml pull
docker compose -f docker-compose.demos.yml up -d --pull always --remove-orphans
docker image prune -f
```

Universal start-or-restart, applied per demo service:

- **not running** → started
- **running an old version** → recreated on the new image
- **already up to date** → left alone (no needless restart)

The script snapshots each service's running image before and after the roll
and prints which of the three outcomes happened. Idempotent — re-running with
the same tag is a no-op.

## Ports

| Service          | Host port | Suggested domain         |
| ---------------- | --------- | ------------------------ |
| `tpsl-demo`      | 3002      | tpsl.example.com         |
| `spot-dca-demo`  | 3003      | spot-dca.example.com     |

## Reverse proxy (Caddy)

Add to `/etc/caddy/Caddyfile`:

```caddyfile
tpsl.example.com {
  reverse_proxy 127.0.0.1:3002
}

spot-dca.example.com {
  reverse_proxy 127.0.0.1:3003
}
```

Reload: `sudo systemctl reload caddy`. Let's Encrypt is automatic.

## Auto-deploy on push (live)

The `deploy` job in `.github/workflows/demos.yml` is already wired up. After the
`build` job finishes, `deploy` copies the compose file + deploy script to the
host via `scp` (through the bastion) and rolls the containers — but **only on a
real push to `new-agents`** (never on PRs, which don't push images).

It deploys the mutable **`latest`** tag. Because only the demo that actually
changed is rebuilt (see [Selective build](#selective-build--caching)), only that
demo's `latest` moves — so `deploy-demos.sh` restarts just that container and
leaves the unchanged one running.

No git is needed on the host: `scp` recreates `/opt/silvana-book-agent` with the
two files the roll needs, so a fresh/empty host works out of the box.

```yaml
  deploy:
    needs: build
    if: github.event_name == 'push' && github.ref == 'refs/heads/new-agents'
    steps:
      - uses: actions/checkout@v4
      - uses: appleboy/scp-action@v1        # copy compose + script to the host
        with: { host, username, key, port, proxy_*, source, target, overwrite }
      - uses: appleboy/ssh-action@v1        # run the roll through the bastion
        with:
          # host / bastion secrets …
          script: |
            cd /opt/silvana-book-agent
            bash scripts/deploy-demos.sh latest
```

### Required repo secrets

Set these under GitHub → repo Settings → Secrets and variables → Actions:

| Secret            | Notes                                             |
| ----------------- | ------------------------------------------------- |
| `SSH_HOST`        | target host running the demos (IP or hostname)    |
| `SSH_USER`        | SSH user on the target host                        |
| `SSH_KEY`         | private key (used for both target and bastion)     |
| `SSH_PORT`        | target SSH port (optional, defaults to 22)         |
| `SSH_PROXY_HOST`  | bastion host to jump through                        |
| `SSH_PROXY_USER`  | bastion SSH user                                    |
| `SSH_PROXY_PORT`  | bastion SSH port (optional, defaults to 22)         |

### One-time host prerequisites

On the deploy host:

1. Docker + compose v2 plugin installed.
2. GHCR pull access — either make the two `*-demo` packages **Public**, or run a
   persistent `docker login ghcr.io` with a `read:packages` PAT (see above).
3. Optional reverse-proxy entries: `tpsl.example.com → 127.0.0.1:3002` and
   `spot-dca.example.com → 127.0.0.1:3003`. Containers are reachable on the host
   ports regardless of the proxy.

The repo does **not** need to be pre-cloned — the `deploy` job `scp`s the compose
file and `scripts/deploy-demos.sh` into `/opt/silvana-book-agent` on every run.

### Alternative: server-side webhook

If you'd rather not give CI SSH access, install
[webhook](https://github.com/adnanh/webhook), point a GitHub push webhook at it,
and run on receipt:

```bash
#!/usr/bin/env bash
set -euo pipefail
cd /opt/silvana-book-agent
git pull --ff-only
bash scripts/deploy-demos.sh latest
```

## Local dev (build without pulling)

Windows / Mac need Docker Desktop with WSL2 or Hyper-V enabled in BIOS.
Linux — just Docker.

```bash
docker compose -f docker-compose.demos.yml up -d --build
```

The `build:` section in `docker-compose.demos.yml` triggers a local
build. The `image:` reference is used as the tag.

## Adding a new demo

Only **two** things, both inside the new demo folder:

1. Scaffold under `crates/agent-<name>/demo/` (mirror the TPSL layout).
2. Give it a `Dockerfile` that declares a **unique** port via `ENV PORT=<n>` and
   `EXPOSE <n>` (pick the next free port — see the Ports table).

Then push. That's it — no workflow, matrix, or compose edits:

- CI auto-discovers `crates/<name>/demo/Dockerfile`, builds + pushes
  `ghcr.io/<owner>/<name>-demo`, and the `deploy` job regenerates the compose
  file and rolls it onto the host (removing orphans, starting the new one).

To preview locally what will be generated:

```bash
bash scripts/discover-demos.sh          # JSON list of discovered demos
bash scripts/gen-demos-compose.sh       # the compose that will be deployed
```

> `docker-compose.demos.yml` in the repo is **generated** — don't hand-edit it.
> Regenerate with `bash scripts/gen-demos-compose.sh > docker-compose.demos.yml`.
