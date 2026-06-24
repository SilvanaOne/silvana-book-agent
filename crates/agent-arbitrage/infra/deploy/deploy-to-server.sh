#!/usr/bin/env bash
# ═══════════════════════════════════════════════════════════════════════════════
# Remote deploy arbitrage-agent: rsync repo → server, docker compose build/up,
# nginx site config generation, Traefik overlay (когда TRAEFIK_ENABLE=1).
#
# Соседи на сервере: host-Traefik (:80/:443), BAM (:8088/:8448), csd-webapp,
# wbuilder, vpn, ollama. Этот скрипт НИКОГДА не трогает чужие контейнеры —
# только свои arbitrage-* и свой Postgres-volume.
#
# Defaults: root@46.250.253.67, domain arbitrage.spcatcher.cfd, /opt/arbitrage-agent.
# Без --force только показывает план и выходит.
# ═══════════════════════════════════════════════════════════════════════════════
set -euo pipefail

for _cmd in ssh rsync; do
  command -v "$_cmd" >/dev/null 2>&1 || { echo "ERROR: ${_cmd} not found." >&2; exit 1; }
done

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"

DEPLOY_SERVER="${DEPLOY_SERVER:-root@46.250.253.67}"
REMOTE_WORK="${REMOTE_WORK:-/opt/arbitrage-agent}"
ARBITRAGE_DOMAIN="${ARBITRAGE_DOMAIN:-arbitrage.spcatcher.cfd}"
UPLOAD_ENV_STACK="${UPLOAD_ENV_STACK:-0}"

FORCE=0
for arg in "$@"; do
  [[ "${arg:-}" == "--force" ]] && FORCE=1
done

if [[ ! -f "${REPO_ROOT}/infra/deploy/docker-compose.yml" ]]; then
  echo "ERROR: infra/deploy/docker-compose.yml not found (REPO_ROOT=${REPO_ROOT})." >&2
  exit 1
fi

cd "${REPO_ROOT}"

echo "════════════════════════════════════════════════════════════"
echo "Host:              ${DEPLOY_SERVER}"
echo "REMOTE_WORK:       ${REMOTE_WORK}"
echo "ARBITRAGE_DOMAIN:  ${ARBITRAGE_DOMAIN}"
echo "UPLOAD_ENV_STACK:  ${UPLOAD_ENV_STACK}"
echo "════════════════════════════════════════════════════════════"

if [[ "${FORCE}" != "1" ]]; then
  cat <<'EOF'

Safe mode: rsync/docker не запускались.

Production deploy:
  export UPLOAD_ENV_STACK=1     # один раз, чтобы залить .env.stack
  ./infra/deploy/deploy-to-server.sh --force

Override: DEPLOY_SERVER, REMOTE_WORK, ARBITRAGE_DOMAIN
EOF
  exit 0
fi

echo "→ rsync repository → ${DEPLOY_SERVER}:${REMOTE_WORK}"
echo "   (infra/deploy/.env.stack исключён — добавь UPLOAD_ENV_STACK=1 чтобы переписать секреты на сервере)"

ssh "${DEPLOY_SERVER}" "mkdir -p '${REMOTE_WORK}'"

rsync -avz --delete \
  --exclude node_modules \
  --exclude .git \
  --exclude .turbo \
  --exclude 'apps/web/.next' \
  --exclude 'apps/api/dist' \
  --exclude 'apps/scanner/dist' \
  --exclude 'packages/*/dist' \
  --exclude '**/*.tsbuildinfo' \
  --exclude 'infra/deploy/.env.stack' \
  "${REPO_ROOT}/" "${DEPLOY_SERVER}:${REMOTE_WORK}/"

if [[ "${UPLOAD_ENV_STACK}" == "1" ]] && [[ -f "${REPO_ROOT}/infra/deploy/.env.stack" ]]; then
  rsync -avz "${REPO_ROOT}/infra/deploy/.env.stack" "${DEPLOY_SERVER}:${REMOTE_WORK}/infra/deploy/.env.stack"
fi

RW_Q="$(printf %q "${REMOTE_WORK}")"
AD_Q="$(printf %q "${ARBITRAGE_DOMAIN}")"

# shellcheck disable=SC2029
ssh "${DEPLOY_SERVER}" "REMOTE_WORK=${RW_Q} ARBITRAGE_DOMAIN=${AD_Q} bash -s" <<'REMOTE'
set -euo pipefail
REMOTE_WORK="${REMOTE_WORK:?}"
ARBITRAGE_DOMAIN="${ARBITRAGE_DOMAIN:?}"
cd "${REMOTE_WORK}"

chmod +x infra/deploy/scripts/ensure-stack-env.sh 2>/dev/null || true
chmod +x infra/deploy/scripts/ensure-gateway-ports.sh 2>/dev/null || true
bash infra/deploy/scripts/ensure-stack-env.sh
bash infra/deploy/scripts/ensure-gateway-ports.sh

# Подставляем домен в nginx-конфиг — plain или ssl в зависимости от наличия letsencrypt.
if [[ -f "/etc/letsencrypt/live/${ARBITRAGE_DOMAIN}/fullchain.pem" ]]; then
  sed "s/__ARBITRAGE_DOMAIN__/${ARBITRAGE_DOMAIN}/g" \
    infra/deploy/nginx/arbitrage-site-ssl.example.conf > infra/deploy/nginx/arbitrage-site-active.conf
else
  sed "s/__ARBITRAGE_DOMAIN__/${ARBITRAGE_DOMAIN}/g" \
    infra/deploy/nginx/arbitrage-site-plain.example.conf > infra/deploy/nginx/arbitrage-site-active.conf
fi

cd infra/deploy
if ! docker compose version >/dev/null 2>&1; then
  echo "ERROR: docker compose v2 plugin missing" >&2
  exit 1
fi

# Жёстко привязываем project name, чтобы compose НЕ воспринял соседние стеки
# (BAM в /opt/batch-order-agent/infra/deploy с тем же basename "deploy") как
# orphans и не удалил их при --remove-orphans.
COMPOSE_PROJECT="arbitrage"

COMPOSE_ENV_ARGS=( --project-name "${COMPOSE_PROJECT}" )
if [[ -f .env.stack ]]; then
  COMPOSE_ENV_ARGS+=( --env-file .env.stack )
fi

COMPOSE_FILES=( -f docker-compose.yml )
TRAEFIK_NET="spgas_net"
if [[ -f .env.stack ]]; then
  _line="$(grep -E '^[[:space:]]*TRAEFIK_EXTERNAL_NETWORK=' .env.stack | tail -1 || true)"
  if [[ -n "${_line}" ]]; then
    TRAEFIK_NET="$(echo "${_line}" | cut -d= -f2- | tr -d '\r' | sed -e 's/^[\"'\'']//' -e 's/[\"'\'']$//')"
  fi
fi
if [[ -f .env.stack ]] && grep -qE '^[[:space:]]*TRAEFIK_ENABLE=(1|true|yes)' .env.stack; then
  if docker network inspect "${TRAEFIK_NET}" >/dev/null 2>&1; then
    COMPOSE_FILES+=( -f docker-compose.traefik.yml )
    echo "[deploy] Traefik overlay enabled → network ${TRAEFIK_NET}"
  else
    echo "WARN: TRAEFIK_ENABLE=1, но network '${TRAEFIK_NET}' не существует — пропускаю Traefik overlay." >&2
  fi
fi

# Стартуем ТОЛЬКО свои сервисы; --remove-orphans действует в рамках project
# 'deploy' (по project-directory) и НЕ трогает чужие compose-проекты на хосте.
docker compose "${COMPOSE_ENV_ARGS[@]}" "${COMPOSE_FILES[@]}" up -d --build --remove-orphans
docker compose "${COMPOSE_ENV_ARGS[@]}" "${COMPOSE_FILES[@]}" ps

echo
echo "Prisma migrations (запустить вручную после первого build):"
echo "  cd ${REMOTE_WORK} && docker compose --project-name arbitrage \\"
echo "    --env-file infra/deploy/.env.stack \\"
echo "    -f infra/deploy/docker-compose.yml exec api \\"
echo "    npx prisma migrate deploy --schema prisma/schema.prisma"
REMOTE

echo "✅ Remote deploy done. Логи: docker logs -f arbitrage-{api,web,scanner,gateway} на сервере."
