#!/usr/bin/env bash
# Локальный запуск arbitrage-agent: инфра (Postgres+Redis), Prisma migrate/generate,
# сборка workspaces, затем turbo dev (api + web + scanner).
#
# По умолчанию скрипт работает В ФОНОВОМ РЕЖИМЕ: запускает агента, выводит
# базовую тех. инфу, показывает рамку со ссылкой на UI и подтверждение, что
# всё в порядке (или ошибки + хвост лога) — и выходит. Терминал свободен,
# логи пишутся в файл.
#
# Подкоманды:
#   ./run.sh                     # запустить агента (фон) + показать баннер
#   ./run.sh stop                # остановить запущенного агента
#   ./run.sh status              # показать состояние
#   ./run.sh logs                # tail -F логов агента
#
# Дополнительные флаги:
#   --prepare-only               # только infra + migrate + build (без запуска dev)
#   --skip-infra                 # форсировать пропуск docker compose
#   --with-infra                 # форсировать docker compose up
#   --no-install                 # пропустить npm install
#   --foreground / --attach      # старое поведение: turbo dev в foreground с живыми логами
#
# Env:
#   ARB_COMPOSE_FILE  — переопределить путь compose (default: infra/docker-compose.yml от корня репо)
#
# Логин в UI отключён для MVP (NEXT_PUBLIC_DISABLE_AUTH=1 + DISABLE_AUTH=1).
# Canton venues идут в mock-режиме; SCANNER_PERSIST=1 — спреды видны сразу.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
COMPOSE_FILE="${ARB_COMPOSE_FILE:-${ROOT}/infra/docker-compose.yml}"

LOGDIR="${ROOT}/logs"
RUN_LOG="${LOGDIR}/run.log"
AGENT_LOG="${LOGDIR}/agent.log"
PIDFILE="${ROOT}/.run/agent.pid"
PORTSFILE="${ROOT}/.run/agent.ports"
DEFAULT_API_PORT=3000
DEFAULT_WEB_PORT=3001

ACTION="run"            # run | stop | status | logs
PREPARE_ONLY=0
NO_INSTALL=0
FOREGROUND=0
INFRA_MODE="auto"       # auto | skip | force

# ── helpers ──────────────────────────────────────────────────────────────────
c_dim()   { printf '\033[2m%s\033[0m' "$*"; }
c_bold()  { printf '\033[1m%s\033[0m' "$*"; }
c_ok()    { printf '\033[32m%s\033[0m' "$*"; }
c_err()   { printf '\033[31m%s\033[0m' "$*"; }
c_warn()  { printf '\033[33m%s\033[0m' "$*"; }
c_accent(){ printf '\033[38;5;208m%s\033[0m' "$*"; }

phase_running() { printf '  %s  %s\n' "$(c_dim '→')" "$1"; }
phase_ok()      { printf '\033[1A\033[2K  %s  %s\n' "$(c_ok '✓')" "$1"; }
phase_fail()    { printf '\033[1A\033[2K  %s  %s\n' "$(c_err '✗')" "$1"; }

run_quiet() {
  local label="$1"; shift
  phase_running "$label"
  if "$@" >> "$RUN_LOG" 2>&1; then
    phase_ok "$label"
  else
    phase_fail "$label"
    echo
    c_err "  Failed — last lines of $RUN_LOG:"; echo
    tail -n 30 "$RUN_LOG" | sed 's/^/    /'
    exit 1
  fi
}

agent_pid() {
  [[ -f "$PIDFILE" ]] || { echo ""; return; }
  local pid; pid="$(cat "$PIDFILE" 2>/dev/null || true)"
  [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null && echo "$pid" || echo ""
}

# Print PID of whatever is bound to a given TCP port (one per line).
pids_on_port() {
  local p="$1"
  if command -v lsof >/dev/null 2>&1; then
    lsof -ti "tcp:${p}" -sTCP:LISTEN 2>/dev/null || true
  elif command -v ss >/dev/null 2>&1; then
    ss -ltnpH "sport = :${p}" 2>/dev/null \
      | grep -oE 'pid=[0-9]+' | head -1 | sed 's/pid=//' || true
  fi
}

# Is the TCP port free on localhost? 0 = free, 1 = busy.
is_port_free() {
  local p="$1"
  # bash builtin TCP connect: success = listener present = busy.
  if (exec 3<>/dev/tcp/127.0.0.1/"$p") 2>/dev/null; then
    exec 3>&- 2>/dev/null
    return 1
  fi
  return 0
}

# Find a free port starting at $1, walk up at most 200 steps. Echoes the port
# or empty string on failure.
find_free_port() {
  local start="$1" p="$1"
  while ! is_port_free "$p"; do
    p=$((p + 1))
    if [[ "$p" -gt $((start + 200)) ]]; then echo ""; return 1; fi
  done
  echo "$p"
}

# Read API_PORT / WEB_PORT from $PORTSFILE; fall back to defaults.
load_ports() {
  api_port="$DEFAULT_API_PORT"
  web_port="$DEFAULT_WEB_PORT"
  if [[ -f "$PORTSFILE" ]]; then
    # shellcheck disable=SC1090
    source "$PORTSFILE" 2>/dev/null || true
    api_port="${API_PORT:-$DEFAULT_API_PORT}"
    web_port="${WEB_PORT:-$DEFAULT_WEB_PORT}"
  fi
}

print_banner() {
  local state="$1"           # RUNNING | STALE | STOPPED
  local web="${2:-$DEFAULT_WEB_PORT}"
  local api="${3:-$DEFAULT_API_PORT}"
  local W=63
  local hr; hr=$(printf '━%.0s' $(seq 1 $W))

  local boxline
  boxline() {
    # plain content padded to W, wrapped in bold ┃ ... ┃
    printf '  \033[1m┃%s┃\033[0m\n' "$(printf "%-${W}s" "$1")"
  }

  echo
  printf '  \033[1m┏%s┓\033[0m\n' "$hr"
  boxline ''
  boxline "   OPEN:    http://localhost:${web}"
  boxline ''
  boxline "   API:     http://localhost:${api}/api/health"
  boxline '   Logs:    ./run.sh logs'
  boxline '   Stop:    ./run.sh stop'
  boxline ''

  # status line: colored state name, but pad the visible (plain) text first.
  local label='   STATUS:  '
  local padded; padded=$(printf '%-12s' "$state")
  # remainder of inner width after label (12) + padded state (12) = 36 spaces
  local rest=$(( W - 12 - 12 ))
  local colour
  case "$state" in
    RUNNING) colour='\033[32m' ;;
    STALE)   colour='\033[33m' ;;
    STOPPED) colour='\033[31m' ;;
    *)       colour='\033[0m' ;;
  esac
  printf '  \033[1m┃%s\033[0m' "$label"
  printf '%b%s\033[0m' "$colour" "$padded"
  printf '%*s\033[1m┃\033[0m\n' "$rest" ''

  printf '  \033[1m┗%s┛\033[0m\n' "$hr"
  echo
}

usage() {
  awk 'NR==1{next} /^#/{sub(/^# ?/,""); print; next} {exit}' "$0"
}

# ── arg parsing ─────────────────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
  case "$1" in
    stop|status|logs) ACTION="$1" ;;
    --prepare-only) PREPARE_ONLY=1 ;;
    --no-install)   NO_INSTALL=1 ;;
    --skip-infra)   INFRA_MODE="skip" ;;
    --with-infra)   INFRA_MODE="force" ;;
    --foreground|--attach) FOREGROUND=1 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Unknown option: $1 (try --help)" >&2; exit 2 ;;
  esac
  shift
done

cd "$ROOT"
mkdir -p "$LOGDIR" "$(dirname "$PIDFILE")"

# ── subcommands stop / status / logs ─────────────────────────────────────────
case "$ACTION" in
  status)
    pid="$(agent_pid)"
    load_ports
    api_up=0; web_up=0
    curl -sf -m 2 "http://localhost:${api_port}/api/health" >/dev/null 2>&1 && api_up=1
    curl -sf -m 2 "http://localhost:${web_port}/"           >/dev/null 2>&1 && web_up=1
    if [[ -n "$pid" ]] && [[ $api_up -eq 1 && $web_up -eq 1 ]]; then
      print_banner 'RUNNING' "$web_port" "$api_port"
      printf '  pid %s · log %s\n' "$pid" "$AGENT_LOG"
      printf '  ports — API :%s %s · Web :%s %s\n\n' \
        "$api_port" "$(c_ok '✓')" "$web_port" "$(c_ok '✓')"
    elif [[ -n "$pid" ]]; then
      print_banner 'STALE' "$web_port" "$api_port"
      printf '  pid %s alive · API :%s %s · Web :%s %s\n\n' \
        "$pid" \
        "$api_port" "$([[ $api_up -eq 1 ]] && c_ok '✓' || c_err '✗')" \
        "$web_port" "$([[ $web_up -eq 1 ]] && c_ok '✓' || c_err '✗')"
      echo "  Tail logs: ./run.sh logs"
      echo
    else
      print_banner 'STOPPED' "$web_port" "$api_port"
    fi
    exit 0
    ;;
  logs)
    [[ -f "$AGENT_LOG" ]] || { echo "no log file at $AGENT_LOG"; exit 1; }
    exec tail -n 200 -F "$AGENT_LOG"
    ;;
  stop)
    pid="$(agent_pid)"
    if [[ -z "$pid" ]]; then
      rm -f "$PIDFILE" "$PORTSFILE"
      echo "  agent is not running"
      exit 0
    fi
    echo "  stopping agent (pid $pid + process group) ..."
    kill -TERM "-$pid" 2>/dev/null || true
    # also try plain TERM in case the leader isn't a group leader on this OS
    kill -TERM "$pid"  2>/dev/null || true
    for _ in $(seq 1 10); do
      kill -0 "$pid" 2>/dev/null || break
      sleep 1
    done
    if kill -0 "$pid" 2>/dev/null; then
      echo "  agent didn't exit cleanly — forcing"
      kill -KILL "-$pid" 2>/dev/null || true
      kill -KILL "$pid"  2>/dev/null || true
    fi
    rm -f "$PIDFILE" "$PORTSFILE"
    c_ok '  ✓ stopped'; echo
    exit 0
    ;;
esac

# ── preflight ────────────────────────────────────────────────────────────────
need_cmd() {
  command -v "$1" >/dev/null 2>&1 || { echo "run.sh: required command not found: $1" >&2; exit 1; }
}
need_cmd node
need_cmd npm

node_major="$(node -p 'Number(process.version.slice(1).split(".")[0])')"
[[ "$node_major" -ge 20 ]] || { echo "run.sh: Node.js 20+ required (have: $(node -v))." >&2; exit 1; }

[[ -f "${ROOT}/package.json" ]] || { echo "run.sh: run from arbitrage_agent root." >&2; exit 1; }

if [[ ! -f "${ROOT}/.env" ]]; then
  if [[ -f "${ROOT}/.env.example" ]]; then
    cp "${ROOT}/.env.example" "${ROOT}/.env"
    echo "  $(c_dim '·') created .env from .env.example"
  else
    echo "run.sh: missing .env and .env.example" >&2
    exit 1
  fi
fi

# load env + dev defaults early so Prisma + every child sees them
set -a; # shellcheck disable=SC1091
source "${ROOT}/.env"; set +a
export SCANNER_PERSIST="${SCANNER_PERSIST:-1}"
export NEXT_PUBLIC_API_URL="${NEXT_PUBLIC_API_URL:-http://localhost:3000}"
export DISABLE_AUTH="${DISABLE_AUTH:-1}"
export NEXT_PUBLIC_DISABLE_AUTH="${NEXT_PUBLIC_DISABLE_AUTH:-1}"

# already running?
existing="$(agent_pid)"
if [[ -n "$existing" ]]; then
  print_banner 'RUNNING'
  echo "  Already running (pid $existing). Use ./run.sh stop first."; echo
  exit 0
fi

# Pick free ports for API and Web. Defaults are 3000/3001; if either is taken
# (by another local project, an older session, etc.) we walk up until something
# is free and use that instead. Both URLs end up in the banner so the operator
# always knows where to point the browser.
API_PORT="$(find_free_port "$DEFAULT_API_PORT")"
WEB_PORT="$(find_free_port "$DEFAULT_WEB_PORT")"
if [[ -z "$API_PORT" || -z "$WEB_PORT" ]]; then
  c_err '  ✗ Could not find a free port in 3000..3200 range'; echo
  exit 1
fi
if [[ "$WEB_PORT" -eq "$API_PORT" ]]; then
  WEB_PORT="$(find_free_port $((WEB_PORT + 1)))"
  [[ -z "$WEB_PORT" ]] && { c_err '  ✗ Could not find a second free port'; echo; exit 1; }
fi
if [[ "$API_PORT" != "$DEFAULT_API_PORT" || "$WEB_PORT" != "$DEFAULT_WEB_PORT" ]]; then
  echo "  $(c_dim '·') default port(s) busy — using API :${API_PORT} / Web :${WEB_PORT}"
fi

# Persist the chosen ports so status/stop/logs know where to look later.
{
  echo "API_PORT=${API_PORT}"
  echo "WEB_PORT=${WEB_PORT}"
} > "$PORTSFILE"

# Children read these from env: apps/api uses PORT, apps/web's dev script
# expands ${WEB_PORT} into `next dev -p`. NEXT_PUBLIC_API_URL is what the
# browser uses to talk to the API, so it must match the chosen API port.
export PORT="$API_PORT"
export WEB_PORT
export NEXT_PUBLIC_API_URL="http://localhost:${API_PORT}"

# fresh run log for this start
: > "$RUN_LOG"

# ── header ───────────────────────────────────────────────────────────────────
echo
echo "  $(c_bold 'arbitrage-agent') $(c_dim "·") $(c_dim "node $(node -v) · $(uname -s)/$(uname -m)")"
echo "  $(c_dim "logs → $RUN_LOG · $AGENT_LOG")"
echo

# ── infra (Postgres + Redis) ────────────────────────────────────────────────
infra_running_count() {
  if ! command -v docker >/dev/null 2>&1; then echo 0; return; fi
  if ! docker compose version >/dev/null 2>&1; then echo 0; return; fi
  [[ -f "$COMPOSE_FILE" ]] || { echo 0; return; }
  docker compose -f "$COMPOSE_FILE" ps --status running --quiet postgres redis 2>/dev/null | wc -l
}

resolved_skip=0
case "$INFRA_MODE" in
  skip)
    resolved_skip=1
    phase_running 'Postgres + Redis'
    phase_ok      'Postgres + Redis (--skip-infra)'
    ;;
  force) resolved_skip=0 ;;
  auto)
    running="$(infra_running_count)"
    if [[ "$running" -ge 2 ]]; then
      resolved_skip=1
      phase_running 'Postgres + Redis'
      phase_ok      'Postgres + Redis (already up)'
    fi
    ;;
esac

if [[ "$resolved_skip" -eq 0 ]]; then
  need_cmd docker
  docker compose version >/dev/null 2>&1 || { echo "  $(c_err '✗') need Docker Compose V2" >&2; exit 1; }
  [[ -f "$COMPOSE_FILE" ]] || { echo "  $(c_err '✗') compose file not found: $COMPOSE_FILE" >&2; exit 1; }
  run_quiet 'Postgres + Redis (docker compose up)' \
    docker compose -f "$COMPOSE_FILE" up -d
  # wait for Postgres
  phase_running 'Postgres ready'
  for _ in $(seq 1 60); do
    docker compose -f "$COMPOSE_FILE" exec -T postgres pg_isready -U postgres -d arbitrage_agent >/dev/null 2>&1 && break
    sleep 1
  done
  phase_ok 'Postgres ready'
fi

# ── npm + Prisma + build ────────────────────────────────────────────────────
if [[ "$NO_INSTALL" -eq 0 ]]; then
  run_quiet 'npm install' npm install
fi

run_quiet 'Prisma migrate deploy' npm run db:migrate:deploy --workspace=@arbitrage-agent/db
run_quiet 'Prisma generate'       npm run db:generate       --workspace=@arbitrage-agent/db
# Always wipe Next.js's prod cache before a fresh build — a half-baked .next/
# from an interrupted previous run otherwise poisons `next dev` (we've seen
# ENOENT routes-manifest.json + MODULE_NOT_FOUND surfacing as GET / 500).
run_quiet 'Clean web cache'       bash -c 'rm -rf apps/web/.next'
run_quiet 'Build workspaces'      npm run build

if [[ "$PREPARE_ONLY" -eq 1 ]]; then
  echo; c_ok '  ✓ preparation done — exiting (--prepare-only)'; echo
  exit 0
fi

# ── launch agent ────────────────────────────────────────────────────────────
if [[ "$FOREGROUND" -eq 1 ]]; then
  echo
  c_warn '  ! foreground mode — turbo dev logs follow (Ctrl+C to stop)'; echo
  echo
  exec npm run dev
fi

# Detached: new session leader so we can kill the whole process group on stop.
# Bash starts a new session via setsid; the wrapper bash PID becomes the leader.
: > "$AGENT_LOG"
setsid bash -c "exec npm run dev >> '$AGENT_LOG' 2>&1" </dev/null &
LEADER_PID=$!
echo "$LEADER_PID" > "$PIDFILE"

phase_running "API boot (:${API_PORT})"
api_ok=0
for _ in $(seq 1 60); do
  if curl -sf -m 2 "http://localhost:${API_PORT}/api/health" >/dev/null 2>&1; then api_ok=1; break; fi
  kill -0 "$LEADER_PID" 2>/dev/null || break
  sleep 1
done
[[ $api_ok -eq 1 ]] && phase_ok "API boot · /api/health 200 OK on :${API_PORT}" || phase_fail "API boot (:${API_PORT})"

phase_running "Web boot (:${WEB_PORT}) — Next.js first compile may take ~30 s"
web_ok=0
if [[ $api_ok -eq 1 ]]; then
  for _ in $(seq 1 90); do
    if curl -sf -m 3 -o /dev/null "http://localhost:${WEB_PORT}/" 2>/dev/null; then web_ok=1; break; fi
    kill -0 "$LEADER_PID" 2>/dev/null || break
    sleep 1
  done
fi
[[ $web_ok -eq 1 ]] && phase_ok "Web boot · :${WEB_PORT} serving" || phase_fail "Web boot (:${WEB_PORT})"

if [[ $api_ok -eq 1 && $web_ok -eq 1 ]]; then
  print_banner 'RUNNING' "$WEB_PORT" "$API_PORT"
  printf '  pid %s · tail logs with %s\n\n' "$LEADER_PID" "$(c_dim './run.sh logs')"
  exit 0
fi

echo
if kill -0 "$LEADER_PID" 2>/dev/null; then
  if [[ $api_ok -eq 0 ]]; then
    c_warn "  agent is alive (pid $LEADER_PID) but the API didn't answer on :${API_PORT}"; echo
  else
    c_warn "  API is up on :${API_PORT} but the web isn't responding on :${WEB_PORT} yet"; echo
    echo "  (Next.js compile may be still running — check ./run.sh logs and refresh.)"
  fi
  echo
  echo "  last 30 lines of $AGENT_LOG:"
  tail -n 30 "$AGENT_LOG" | sed 's/^/    /'
  echo
  echo "  re-check with $(c_dim ./run.sh status), stop with $(c_dim ./run.sh stop)"
  print_banner 'STALE' "$WEB_PORT" "$API_PORT"
  exit 1
else
  c_err "  agent process exited during boot"; echo
  echo "  last 40 lines of $AGENT_LOG:"
  tail -n 40 "$AGENT_LOG" | sed 's/^/    /'
  echo
  rm -f "$PIDFILE" "$PORTSFILE"
  print_banner 'STOPPED' "$WEB_PORT" "$API_PORT"
  exit 1
fi
