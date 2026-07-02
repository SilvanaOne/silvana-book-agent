#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# scripts/scaffold-demo.sh — clone an existing agent demo to a new one,
# renaming identifiers, files, port, brand letter and title in one shot.
#
# The source of truth is the mean-reversion demo (crates/agent-mean-reversion/
# demo). This script produces a fully self-contained sibling under
# crates/agent-<slug>/demo. The demo pipeline picks it up automatically
# because discover-demos.sh looks for `crates/*/demo/Dockerfile`.
#
# What gets rewritten (case-sensitive):
#   * paths          crates/agent-mean-reversion  → crates/agent-<slug>
#   * package name   agent-mean-reversion-demo    → agent-<slug>-demo
#   * api namespace  /api/mr/                     → /api/<slug>/
#   * engine file    mr-engine.ts                 → <short>-engine.ts
#   * store globals  __mrDemoStore                → __<short>DemoStore
#   * types          MrState/MrConfig/MrOrder     → <Pascal>State/Config/Order
#   * components     MrForm.tsx / MrChart.tsx     → <Pascal>Form.tsx / Chart.tsx
#   * fns            startMr / stopMr             → start<Pascal> / stop<Pascal>
#   * bare vars      \bmr\b                       → \b<short>\b   (word-bounded)
#   * brand          Mean Reversion / -           → <title>
#   * layout meta    agent-mean-reversion demo    → agent-<slug> demo
#   * port           3004                         → <port>
#   * brand mark     <span className="brand-mark">M</span> → <letter>
#
# <short> and <Pascal> are derived from <slug>:
#   spot-grid → short=spotgrid, Pascal=SpotGrid
#   twap      → short=twap,     Pascal=Twap
#
# Usage:
#   bash scripts/scaffold-demo.sh <slug> <port> <letter> "<title>"
#
# Example:
#   bash scripts/scaffold-demo.sh twap 3005 T "TWAP"
#   bash scripts/scaffold-demo.sh spot-grid 3006 G "Spot Grid"
# ---------------------------------------------------------------------------
set -euo pipefail

usage() {
  cat >&2 <<EOF
usage: $0 <slug> <port> <letter> "<title>" [--from <source-slug>]

  <slug>   kebab-case, e.g. twap, spot-grid
  <port>   TCP port (3005+; must be unique across demos)
  <letter> single character for the topbar brand mark
  <title>  brand title, e.g. "TWAP" or "Spot Grid"
  --from   source demo slug to clone from (default: mean-reversion)
EOF
  exit 1
}

[[ $# -ge 4 ]] || usage
SLUG="$1"; shift
PORT="$1"; shift
LETTER="$1"; shift
TITLE="$1"; shift
FROM="mean-reversion"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --from) FROM="$2"; shift 2 ;;
    *) echo "unknown arg: $1" >&2; usage ;;
  esac
done

if [[ ! "$SLUG" =~ ^[a-z][a-z0-9-]*$ ]]; then
  echo "ERROR: <slug> must be kebab-case ([a-z0-9-]+ starting with a letter): '$SLUG'" >&2
  exit 1
fi
if [[ ! "$PORT" =~ ^[0-9]+$ ]] || (( PORT < 1024 || PORT > 65535 )); then
  echo "ERROR: <port> must be an integer in 1024..65535: '$PORT'" >&2
  exit 1
fi
if (( ${#LETTER} != 1 )); then
  echo "ERROR: <letter> must be exactly one character: '$LETTER'" >&2
  exit 1
fi

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC="$ROOT/crates/agent-$FROM/demo"
DEST_CRATE="$ROOT/crates/agent-$SLUG"
DEST="$DEST_CRATE/demo"

[[ -d "$SRC" ]]   || { echo "ERROR: source demo not found: $SRC" >&2; exit 1; }
[[ ! -e "$DEST" ]] || { echo "ERROR: destination already exists: $DEST" >&2; exit 1; }

# Derive short + Pascal from slug (and matching pair for the source).
to_short()  { echo "$1" | tr -d '-'; }
to_pascal() { echo "$1" | awk -F'-' '{for(i=1;i<=NF;i++){$i=toupper(substr($i,1,1)) substr($i,2)}}1' OFS=''; }

SHORT="$(to_short "$SLUG")"
PASCAL="$(to_pascal "$SLUG")"
# The clone source's own tokens. For mean-reversion these are "mr"/"Mr".
# NOTE: keep the source demo self-consistent — if you clone from another
# demo whose tokens differ, override via FROM_SHORT/FROM_PASCAL below.
case "$FROM" in
  mean-reversion) FROM_SHORT="mr";     FROM_PASCAL="Mr" ;;
  *)              FROM_SHORT="$(to_short "$FROM")"; FROM_PASCAL="$(to_pascal "$FROM")" ;;
esac

echo "→ Scaffolding crates/agent-$SLUG/demo"
echo "    from   : crates/agent-$FROM/demo"
echo "    port   : $PORT"
echo "    letter : $LETTER"
echo "    title  : $TITLE"
echo "    short  : $SHORT   (was $FROM_SHORT)"
echo "    Pascal : $PASCAL  (was $FROM_PASCAL)"

# 1. Copy the source demo, skipping generated dirs.
mkdir -p "$DEST_CRATE"
cp -r "$SRC" "$DEST"
rm -rf "$DEST/node_modules" "$DEST/.next"

# 2. Rename source-specific files.
if [[ -f "$DEST/lib/${FROM_SHORT}-engine.ts" ]]; then
  mv "$DEST/lib/${FROM_SHORT}-engine.ts" "$DEST/lib/${SHORT}-engine.ts"
fi
if [[ -f "$DEST/app/components/${FROM_PASCAL}Form.tsx" ]]; then
  mv "$DEST/app/components/${FROM_PASCAL}Form.tsx" "$DEST/app/components/${PASCAL}Form.tsx"
fi
if [[ -f "$DEST/app/components/${FROM_PASCAL}Chart.tsx" ]]; then
  mv "$DEST/app/components/${FROM_PASCAL}Chart.tsx" "$DEST/app/components/${PASCAL}Chart.tsx"
fi
if [[ -d "$DEST/app/api/${FROM_SHORT}" ]]; then
  mv "$DEST/app/api/${FROM_SHORT}" "$DEST/app/api/${SLUG}"
fi

# 3. Content substitutions across text files. Order matters: run
#    longest / most-specific patterns before the bare-token ones.
find "$DEST" -type f \( \
     -name '*.ts' -o -name '*.tsx' -o -name '*.json' -o -name '*.css' \
  -o -name 'Dockerfile' -o -name '.dockerignore' -o -name '.gitignore' \
\) -print0 | \
  xargs -0 sed -i \
    -e "s|agent-${FROM}-demo|agent-${SLUG}-demo|g" \
    -e "s|agent-${FROM}|agent-${SLUG}|g" \
    -e "s|${FROM}-demo|${SLUG}-demo|g" \
    -e "s|${FROM}|${SLUG}|g" \
    -e "s|${FROM_SHORT}-engine|${SHORT}-engine|g" \
    -e "s|/api/${FROM_SHORT}/|/api/${SLUG}/|g" \
    -e "s|${FROM_PASCAL}Form|${PASCAL}Form|g" \
    -e "s|${FROM_PASCAL}Chart|${PASCAL}Chart|g" \
    -e "s|${FROM_PASCAL}State|${PASCAL}State|g" \
    -e "s|${FROM_PASCAL}Config|${PASCAL}Config|g" \
    -e "s|${FROM_PASCAL}Order|${PASCAL}Order|g" \
    -e "s|start${FROM_PASCAL}|start${PASCAL}|g" \
    -e "s|stop${FROM_PASCAL}|stop${PASCAL}|g" \
    -e "s|__${FROM_SHORT}DemoStore|__${SHORT}DemoStore|g" \
    -e "s|\\b${FROM_SHORT}\\b|${SHORT}|g" \
    -e "s|Mean Reversion|${TITLE}|g" \
    -e "s|Mean-Reversion|${TITLE}|g" \
    -e "s|ENV PORT=3004|ENV PORT=${PORT}|g" \
    -e "s|EXPOSE 3004|EXPOSE ${PORT}|g" \
    -e "s|-p 3004|-p ${PORT}|g" \
    -e "s|3004:3004|${PORT}:${PORT}|g"

# 4. Brand mark letter in TopBar (single char, keep it explicit).
if [[ -f "$DEST/app/components/TopBar.tsx" ]]; then
  sed -i "s|<span className=\"brand-mark\">.</span>|<span className=\"brand-mark\">${LETTER}</span>|" \
      "$DEST/app/components/TopBar.tsx"
fi

# 5. Sanity check that the discover pipeline sees the new demo.
if [[ -x "$ROOT/scripts/discover-demos.sh" ]]; then
  if ! bash "$ROOT/scripts/discover-demos.sh" | grep -q "\"$SLUG-demo\""; then
    echo "WARN: discover-demos.sh did not find $SLUG-demo — check ENV PORT / EXPOSE in the Dockerfile" >&2
  fi
fi

echo "✔ Done: $DEST"
echo "  Next: cd $DEST && npm ci && npm run build"
echo "  Then: edit lib/${SHORT}-engine.ts + ${PASCAL}Form.tsx + ${PASCAL}Chart.tsx + InfoGrid.tsx for agent-specific logic."
