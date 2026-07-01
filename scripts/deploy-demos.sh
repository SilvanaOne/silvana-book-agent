#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# scripts/deploy-demos.sh — pull the latest built images and roll containers.
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

docker compose -f "$COMPOSE_FILE" pull
docker compose -f "$COMPOSE_FILE" up -d --remove-orphans
docker image prune -f

echo "✔ Deployed"
docker compose -f "$COMPOSE_FILE" ps
