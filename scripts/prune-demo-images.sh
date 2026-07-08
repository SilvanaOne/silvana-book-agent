#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# scripts/prune-demo-images.sh — delete GHCR demo images that no longer exist
# in git. A demo image is named "<short>-demo"; the authoritative set is what
# scripts/discover-demos.sh finds. Any *-demo container package owned by $OWNER
# that is NOT in that set (e.g. a renamed-away or deleted demo) is removed.
#
# Only *-demo packages are ever considered — agent binary images and any other
# packages are never touched.
#
# Requires: gh CLI authenticated (GH_TOKEN) with a token that has BOTH
# read:packages and delete:packages. Env:
#   OWNER     org/user that owns the packages (default: silvanaone)
#   REPO      repo whose packages are in scope (default: silvana-book-agent) —
#             ONLY packages linked to this repo are ever touched, never the whole
#             organization.
#   DRY_RUN   "1" → only print what would be deleted
# ---------------------------------------------------------------------------
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
owner="${OWNER:-silvanaone}"
repo="${REPO:-silvana-book-agent}"
dry="${DRY_RUN:-0}"

# Demo image names currently in git (source of truth).
valid="$(bash "$here/discover-demos.sh" | jq -r '.[].name' | sort -u)"
echo "Valid demos in git (${owner}/${repo}):"
echo "$valid" | sed 's/^/  /'

# Org vs user determines the API paths.
if gh api "/orgs/${owner}" >/dev/null 2>&1; then
  list_path="/orgs/${owner}/packages?package_type=container&per_page=100"
  del_base="/orgs/${owner}/packages/container"
else
  list_path="/users/${owner}/packages?package_type=container&per_page=100"
  del_base="/user/packages/container"   # user API only deletes your own packages
fi

# Container packages that belong to THIS repo only (filter by the package's
# linked repository — packages of other repos in the org are never listed here).
mapfile -t pkgs < <(gh api --paginate "$list_path" \
  --jq ".[] | select((.repository.name // \"\") == \"${repo}\") | .name" 2>/dev/null || true)

to_delete=()
for p in "${pkgs[@]:-}"; do
  [ -n "$p" ] || continue
  case "$p" in
    *-demo) ;;            # only demo images are in scope
    *) continue ;;
  esac
  grep -qxF "$p" <<<"$valid" || to_delete+=("$p")
done

if [ "${#to_delete[@]}" -eq 0 ]; then
  echo "✔ Nothing to prune — registry matches git."
  exit 0
fi

echo "Stale demo packages (in registry, gone from git):"
printf '  - %s\n' "${to_delete[@]}"

if [ "$dry" = "1" ]; then
  echo "(dry-run) not deleting."
  exit 0
fi

fail=0
for p in "${to_delete[@]}"; do
  echo "→ deleting ${p}"
  if gh api --method DELETE "${del_base}/${p}" >/dev/null 2>&1; then
    echo "  ✔ deleted"
  else
    echo "  ✖ failed to delete ${p} (token needs delete:packages?)"
    fail=1
  fi
done

exit "$fail"
