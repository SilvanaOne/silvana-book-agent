#!/usr/bin/env bash
# Локальный запуск batch-order-agent: инфра (Postgres+Redis), Prisma migrate/seed, сборка workspaces, затем turbo dev (api + web + worker).
#
# Использование:
#   ./run.sh                     # авто: если postgres+redis уже работают — пропустить infra, иначе поднять
#   ./run.sh --prepare-only      # только infra + migrate + seed + build (без dev)
#   ./run.sh --no-seed           # без prisma db seed
#   ./run.sh --skip-infra        # форсировать пропуск docker compose (Postgres/Redis вы держите сами)
#   ./run.sh --with-infra        # форсировать docker compose up (даже если контейнеры уже бегут — будут пересозданы при изменениях)
#
# Env:
#   BAM_COMPOSE_FILE  — переопределить путь compose (default: infra/docker-compose.yml от корня репо)

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
COMPOSE_FILE="${BAM_COMPOSE_FILE:-${ROOT}/infra/docker-compose.yml}"

PREPARE_ONLY=0
NO_SEED=0
INFRA_MODE="auto"   # auto | skip | force

while [[ $# -gt 0 ]]; do
  case "$1" in
    --prepare-only) PREPARE_ONLY=1 ;;
    --no-seed) NO_SEED=1 ;;
    --skip-infra) INFRA_MODE="skip" ;;
    --with-infra) INFRA_MODE="force" ;;
    -h|--help)
      grep '^#' "$0" | grep -v '#!/usr/bin' | sed 's/^# \{0,1\}//'
      exit 0 ;;
    *)
      echo "Unknown option: $1 (try --help)" >&2
      exit 2 ;;
  esac
  shift
done

cd "$ROOT"

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "run.sh: required command not found: $1" >&2
    exit 1
  }
}

need_cmd node
need_cmd npm

node_major="$(node -p 'Number(process.version.slice(1).split(".")[0])')"
if [[ "$node_major" -lt 18 ]]; then
  echo "run.sh: Node.js 18+ required (have: $(node -v))." >&2
  exit 1
fi

if [[ ! -f "${ROOT}/package.json" ]]; then
  echo "run.sh: run from batch-order-agent root." >&2
  exit 1
fi

if [[ ! -f "${ROOT}/.env" ]]; then
  if [[ -f "${ROOT}/.env.example" ]]; then
    cp "${ROOT}/.env.example" "${ROOT}/.env"
    echo "run.sh: created .env from .env.example — review secrets (SILVANA_JWT, etc.)."
  else
    echo "run.sh: missing .env and .env.example" >&2
    exit 1
  fi
fi

wait_for_pg() {
  local i=0
  echo "run.sh: waiting for PostgreSQL..."
  until docker compose -f "$COMPOSE_FILE" exec -T postgres pg_isready -U postgres -d batch_agent >/dev/null 2>&1; do
    i=$((i + 1))
    if [[ $i -gt 90 ]]; then
      echo "run.sh: timeout waiting for postgres (docker compose)." >&2
      exit 1
    fi
    sleep 1
  done
  echo "run.sh: PostgreSQL is accepting connections."
}

# Decide whether to bring up the local infra (postgres + redis).
# - skip:  user asked to skip outright (--skip-infra).
# - force: user asked to bring it up unconditionally (--with-infra).
# - auto:  bring up only if both services are NOT already running for this compose project.
infra_running_count() {
  if ! command -v docker >/dev/null 2>&1; then
    echo 0
    return
  fi
  if ! docker compose version >/dev/null 2>&1; then
    echo 0
    return
  fi
  if [[ ! -f "$COMPOSE_FILE" ]]; then
    echo 0
    return
  fi
  # --status running --quiet — IDs of services that are actually up. We ask for postgres+redis by name.
  docker compose -f "$COMPOSE_FILE" ps --status running --quiet postgres redis 2>/dev/null | wc -l
}

resolved_skip=0
case "$INFRA_MODE" in
  skip)
    echo "run.sh: --skip-infra (ensure Postgres and Redis match DATABASE_URL / REDIS_URL in .env)"
    resolved_skip=1
    ;;
  force)
    echo "run.sh: --with-infra → forcing docker compose up regardless of current state"
    resolved_skip=0
    ;;
  auto)
    running="$(infra_running_count)"
    if [[ "$running" -ge 2 ]]; then
      echo "run.sh: auto-detect → postgres+redis already running for this compose project, skipping infra start"
      resolved_skip=1
    else
      echo "run.sh: auto-detect → postgres or redis not running ($running/2), will bring infra up"
      resolved_skip=0
    fi
    ;;
esac

if [[ "$resolved_skip" -eq 0 ]]; then
  need_cmd docker
  if ! docker compose version >/dev/null 2>&1; then
    echo "run.sh: need Docker Compose V2 (\`docker compose\`)." >&2
    exit 1
  fi
  if [[ ! -f "$COMPOSE_FILE" ]]; then
    echo "run.sh: compose file not found: $COMPOSE_FILE" >&2
    exit 1
  fi
  echo "run.sh: starting Postgres + Redis ($COMPOSE_FILE)..."
  docker compose -f "$COMPOSE_FILE" up -d
  wait_for_pg
fi

echo "run.sh: npm install..."
npm install

echo "run.sh: Prisma migrate deploy + generate..."
npm run db:migrate:apply

if [[ "$NO_SEED" -eq 0 ]]; then
  echo "run.sh: prisma db seed..."
  npm run db:seed || {
    echo "run.sh: db:seed failed (DB may already be usable). Fix errors if this is unexpected." >&2
    exit 1
  }
else
  echo "run.sh: skipping seed (--no-seed)"
fi

echo "run.sh: building workspaces (deps need dist/ for tsx/next)..."
npm run build

echo "run.sh: preparation done."

if [[ "$PREPARE_ONLY" -eq 1 ]]; then
  echo "run.sh: --prepare-only set; exiting without dev."
  exit 0
fi

cat <<'BANNER'

┏━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┓
┃                                                                           ┃
┃    OPEN IN BROWSER:                                                       ┃
┃                                                                           ┃
┃        →   http://localhost:3001                                          ┃
┃                                                                           ┃
┃    API:    http://localhost:3000                                          ┃
┃    Worker health:  http://localhost:3002                                  ┃
┃                                                                           ┃
┃    Stop:   Ctrl+C                                                         ┃
┃                                                                           ┃
┗━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛

BANNER

# Next dev resolves .env relative to apps/web (cwd of the child process), not the monorepo root.
# Export everything from the root .env so api / web / worker children all inherit the same values.
# Required for NEXT_PUBLIC_* (baked into Next dev client bundle), DEMO_TOOLS (api gate), etc.
if [[ -f "${ROOT}/.env" ]]; then
  set -a
  # shellcheck disable=SC1091
  source "${ROOT}/.env"
  set +a
fi

exec npm run dev
