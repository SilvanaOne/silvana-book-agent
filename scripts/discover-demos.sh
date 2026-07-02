#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# scripts/discover-demos.sh — auto-discover every agent demo in the repo.
#
# A "demo" is any `crates/agent-<name>/demo/` directory that contains a
# Dockerfile. Nothing is hard-coded: drop in a new demo folder with a
# Dockerfile that declares its port and it is picked up automatically.
#
# Emits a JSON array to stdout, one object per demo:
#   [{"name":"tpsl-demo","short":"tpsl","context":"crates/agent-tpsl/demo","port":3002}, ...]
#
#   name    — image / compose service name  (<short>-demo)
#   short   — agent name without the `agent-` prefix
#   context — build context (the demo dir)
#   port    — host+container port, read from the Dockerfile (ENV PORT= / EXPOSE)
# ---------------------------------------------------------------------------
set -euo pipefail

cd "$(cd "$(dirname "$0")/.." && pwd)"

items=()
while IFS= read -r df; do
  dir="${df%/Dockerfile}"                        # crates/agent-tpsl/demo
  agent="$(basename "$(dirname "$dir")")"        # agent-tpsl
  short="${agent#agent-}"                         # tpsl
  name="${short}-demo"

  # Port is the single source of truth in each demo's Dockerfile.
  port="$(grep -ioE '(ENV[[:space:]]+PORT[[:space:]=]+|EXPOSE[[:space:]]+)[0-9]+' "$df" \
            | grep -oE '[0-9]+' | head -n1 || true)"
  if [ -z "$port" ]; then
    echo "WARN: no ENV PORT / EXPOSE in $df — skipping '$name'" >&2
    continue
  fi

  items+=("{\"name\":\"$name\",\"short\":\"$short\",\"context\":\"$dir\",\"port\":$port}")
done < <(find crates -maxdepth 4 -type f -path '*/demo/Dockerfile' | sort)

# Join into a JSON array (empty → []).
if [ "${#items[@]}" -eq 0 ]; then
  printf '[]\n'
else
  printf '[%s]\n' "$(IFS=,; echo "${items[*]}")"
fi
