#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# scripts/deploy-demos.sh — pull the requested images and roll containers.
#
# Universal start-or-restart semantics (per demo service):
#   - not running        → start it
#   - running old version → recreate it on the new image
#   - already up to date  → leave it alone (no needless restart)
#
# All three are handled by `docker compose up -d --pull always`: compose
# diffs the desired image against each running container and only recreates
# the ones whose image actually changed, while starting any that are missing.
# `--pull always` guarantees we detect a new build even when the tag string
# is reused (e.g. `latest`); a pinned `sha-<short>` tag changes the reference
# anyway, so the recreate is doubly certain.
#
# Assumes:
#   - Docker + docker compose plugin installed on the host
#   - This repo cloned somewhere (e.g. /opt/silvana-book-agent) — used only
#     for docker-compose.demos.yml; source is NOT rebuilt on the host
#   - Optionally: GHCR pull requires `docker login ghcr.io` first (public
#     images don't need it)
#
# Usage:
#   bash scripts/deploy-demos.sh                # deploy latest tag
#   bash scripts/deploy-demos.sh sha-abc1234    # deploy a specific tag
#
# Wire this to a GitHub webhook, a systemd timer, or `pm2 deploy`.
# ---------------------------------------------------------------------------
set -euo pipefail

TAG="${1:-latest}"
COMPOSE_FILE="$(cd "$(dirname "$0")/.." && pwd)/docker-compose.demos.yml"

echo "→ Deploying demos with tag='${TAG}' from ${COMPOSE_FILE}"

export DEMO_TAG="${TAG}"

compose() { docker compose -f "$COMPOSE_FILE" "$@"; }

# Snapshot the currently-running container image per service so we can report
# exactly what started / restarted / stayed put after the roll.
declare -A before
for svc in $(compose config --services); do
  cid="$(compose ps -q "$svc" 2>/dev/null || true)"
  if [[ -n "$cid" ]]; then
    before[$svc]="$(docker inspect -f '{{.Image}}' "$cid" 2>/dev/null || echo '')"
  else
    before[$svc]=""
  fi
done

# Fetch the requested tag, then start-or-recreate. `up -d` starts missing
# containers and recreates only those whose image changed; `--pull always`
# forces a fresh image check; `--remove-orphans` is scoped to this compose
# project ("silvana-book-agent"), so neighbour stacks on the host are untouched.
compose pull
compose up -d --pull always --remove-orphans

# Report per-service outcome.
echo "→ Result:"
for svc in $(compose config --services); do
  cid="$(compose ps -q "$svc" 2>/dev/null || true)"
  after=""
  [[ -n "$cid" ]] && after="$(docker inspect -f '{{.Image}}' "$cid" 2>/dev/null || echo '')"
  if [[ -z "${before[$svc]:-}" && -n "$after" ]]; then
    echo "   • ${svc}: started (was not running)"
  elif [[ -n "${before[$svc]:-}" && "${before[$svc]}" != "$after" ]]; then
    echo "   • ${svc}: restarted on new version"
  else
    echo "   • ${svc}: already up to date (no change)"
  fi
done

docker image prune -f

echo "✔ Deployed"
compose ps
