#!/usr/bin/env bash
# Minimal smoke: GET /api/venues/status (curl). No Playwright needed for JSON API checks.
#
# Env:
#   SMOKE_API_BASE_URL — default http://127.0.0.1:3000
#   API_INTERNAL_KEY   — if apps/api enables internal auth, set this (same as deployment)
#
set -euo pipefail

BASE_URL="${SMOKE_API_BASE_URL:-http://127.0.0.1:3000}"
TARGET="${BASE_URL%/}/api/venues/status"

CURL_OPTS=(-sS -f --connect-timeout 5 --max-time 30)
HDRS=(-H "Accept: application/json")
if [[ -n "${API_INTERNAL_KEY:-}" ]]; then
  HDRS+=(-H "Authorization: Bearer ${API_INTERNAL_KEY}")
fi

body="$(curl "${CURL_OPTS[@]}" "${HDRS[@]}" "$TARGET")"

printf '%s' "$body" | node -e '
const fs = require("node:fs");
const j = JSON.parse(fs.readFileSync(0, "utf8"));
if (typeof j.checkedAt !== "string") process.exit(1);
if (typeof j.executionRouterEnabled !== "boolean") process.exit(1);
if (!Array.isArray(j.venues) || j.venues.length < 1) process.exit(1);
for (const v of j.venues) {
  if (typeof v.venue !== "string" || typeof v.configured !== "boolean") process.exit(1);
}
'

echo "smoke-venues-status: OK (${TARGET})"
