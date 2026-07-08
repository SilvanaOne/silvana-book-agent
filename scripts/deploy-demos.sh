#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# scripts/deploy-demos.sh — pull the requested images and roll containers.
#
# Universal start-or-restart semantics (per demo service):
#   - not running        → start it
#   - running old version → recreate it on the new image
#   - already up to date  → leave it alone (no needless restart)
#   - image missing in registry → skip it (don't fail the whole roll)
#
# The first three are handled by `docker compose up -d`: compose diffs the
# desired image against each running container and only recreates the ones
# whose image actually changed, while starting any that are missing.
#
# The last point matters when the demo set changes (a demo was renamed or newly
# added but its image hasn't been built+pushed yet): `docker compose up` on ALL
# services would abort with "manifest not found" for the missing ones. So we
# pull with --ignore-pull-failures and then bring up ONLY the services whose
# image is actually available locally. Missing ones are logged and skipped;
# they roll in automatically once the build job publishes them.
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

# Fetch every image, tolerating ones that don't exist yet in the registry.
compose pull --ignore-pull-failures || true

# Select only the services whose image is now present locally. Services whose
# image is missing (never built / just renamed) are skipped, not fatal.
avail=()
skipped=()
for svc in $(compose config --services); do
  img="$(compose config --images "$svc" 2>/dev/null | head -n1 || true)"
  if [[ -n "$img" ]] && docker image inspect "$img" >/dev/null 2>&1; then
    avail+=("$svc")
  else
    skipped+=("$svc")
  fi
done

if [[ "${#skipped[@]}" -gt 0 ]]; then
  echo "⚠ Skipping ${#skipped[@]} demo(s) with no image in the registry yet:"
  printf '   - %s\n' "${skipped[@]}"
fi

if [[ "${#avail[@]}" -eq 0 ]]; then
  echo "✖ No demo images available to deploy — nothing to do." >&2
  exit 0
fi

# Start-or-recreate only the available services. `--remove-orphans` drops
# containers for services no longer in the compose file (e.g. renamed-away),
# scoped to this compose project so neighbour stacks are untouched.
compose up -d --remove-orphans "${avail[@]}"

# Report per-service outcome.
echo "→ Result:"
for svc in $(compose config --services); do
  cid="$(compose ps -q "$svc" 2>/dev/null || true)"
  after=""
  [[ -n "$cid" ]] && after="$(docker inspect -f '{{.Image}}' "$cid" 2>/dev/null || echo '')"
  if [[ " ${skipped[*]} " == *" $svc "* ]]; then
    echo "   • ${svc}: skipped (no image)"
  elif [[ -z "${before[$svc]:-}" && -n "$after" ]]; then
    echo "   • ${svc}: started (was not running)"
  elif [[ -n "${before[$svc]:-}" && "${before[$svc]}" != "$after" ]]; then
    echo "   • ${svc}: restarted on new version"
  else
    echo "   • ${svc}: already up to date (no change)"
  fi
done

docker image prune -f

echo "✔ Deployed ${#avail[@]} demo(s)"
compose ps
