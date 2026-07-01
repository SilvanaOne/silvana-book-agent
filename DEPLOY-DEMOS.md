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

That script does:

```
docker compose -f docker-compose.demos.yml pull
docker compose -f docker-compose.demos.yml up -d --remove-orphans
docker image prune -f
```

Idempotent. Only recreates containers whose image tag changed.

## Ports

| Service          | Host port | Suggested domain         |
| ---------------- | --------- | ------------------------ |
| `tpsl-demo`      | 3002      | tpsl.spcatcher.cfd       |
| `spot-dca-demo`  | 3003      | spot-dca.spcatcher.cfd   |

## Reverse proxy (Caddy)

Add to `/etc/caddy/Caddyfile`:

```caddyfile
tpsl.spcatcher.cfd {
  reverse_proxy 127.0.0.1:3002
}

spot-dca.spcatcher.cfd {
  reverse_proxy 127.0.0.1:3003
}
```

Reload: `sudo systemctl reload caddy`. Let's Encrypt is automatic.

## Auto-deploy on push

Two flavors, pick one.

### 1) GitHub webhook → tiny hook on the server

Install [webhook](https://github.com/adnanh/webhook), point a GitHub
webhook at it, run this on receipt:

```bash
#!/usr/bin/env bash
set -euo pipefail
cd /opt/silvana-book-agent
git pull --ff-only
bash scripts/deploy-demos.sh latest
```

### 2) GitHub Actions SSH deploy

Add a `deploy` job to `.github/workflows/demos.yml` that runs after the
image build succeeds:

```yaml
  deploy:
    needs: build
    runs-on: ubuntu-latest
    if: github.ref == 'refs/heads/new-agents'
    steps:
      - uses: appleboy/ssh-action@v1
        with:
          host: ${{ secrets.SSH_HOST }}
          username: ${{ secrets.SSH_USER }}
          key: ${{ secrets.SSH_KEY }}
          script: |
            cd /opt/silvana-book-agent
            git pull --ff-only
            bash scripts/deploy-demos.sh sha-${{ github.sha }}
```

Requires `SSH_HOST`, `SSH_USER`, `SSH_KEY` set in repo Secrets.

## Local dev (build without pulling)

Windows / Mac need Docker Desktop with WSL2 or Hyper-V enabled in BIOS.
Linux — just Docker.

```bash
docker compose -f docker-compose.demos.yml up -d --build
```

The `build:` section in `docker-compose.demos.yml` triggers a local
build. The `image:` reference is used as the tag.

## Adding a new demo

1. Scaffold under `crates/agent-<name>/demo/` (mirror the TPSL layout).
2. Copy the Dockerfile — change **two** `PORT` / `EXPOSE` values to the
   next free port.
3. Add a service block to `docker-compose.demos.yml`:

   ```yaml
     <name>-demo:
       image: ${<NAME>_IMAGE:-ghcr.io/silvanaone/<name>-demo:${DEMO_TAG:-latest}}
       build:
         context: ./crates/agent-<name>/demo
       container_name: <name>-demo
       restart: unless-stopped
       ports:
         - "<PORT>:<PORT>"
   ```

4. Add a matrix entry to `.github/workflows/demos.yml`:

   ```yaml
             - name: <name>-demo
               context: crates/agent-<name>/demo
   ```

5. Push. GHA builds + pushes the image. On the server:
   `bash scripts/deploy-demos.sh` picks it up.
