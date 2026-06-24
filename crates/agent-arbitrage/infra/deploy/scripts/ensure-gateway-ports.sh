#!/usr/bin/env bash
# Если на host'е порты 80/443 уже заняты ДРУГИМ сервисом (например, host-Traefik
# или BAM-gateway), подберём следующие свободные и допишем в .env.stack.
# В режиме Traefik (TRAEFIK_ENABLE=1) external-traffic заходит через Traefik
# по network spgas_net, и публикация на хост-порту вообще не нужна — fallback'ы
# нужны только если хочется dial gateway напрямую по http://SERVER:<port>.
#
# Skip: DISABLE_GATEWAY_PORT_AUTOPICK=1 или .env.stack отсутствует.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INFRA_DEPLOY="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${INFRA_DEPLOY}"

if [[ "${DISABLE_GATEWAY_PORT_AUTOPICK:-0}" == "1" ]]; then
  echo "[ensure-gateway-ports] disabled (DISABLE_GATEWAY_PORT_AUTOPICK=1)"
  exit 0
fi

ENV_FILE="${INFRA_DEPLOY}/.env.stack"
if [[ ! -f "${ENV_FILE}" ]]; then
  echo "[ensure-gateway-ports] no ${ENV_FILE}, skip"
  exit 0
fi

if ! command -v ss >/dev/null 2>&1; then
  echo "[ensure-gateway-ports] ss not found, skip"
  exit 0
fi

port_listening() {
  local p="${1:?}"
  ss -tln 2>/dev/null | grep -qE ":${p}[[:space:]]"
}

our_gateway_uses_host_port() {
  local p="${1:?}"
  docker ps --filter "name=^arbitrage-gateway$" --format '{{.Ports}}' 2>/dev/null \
    | grep -qE "(^| )0\.0\.0\.0:${p}->|:::${p}->" || return 1
  return 0
}

# 8089/8449 чтобы не сталкиваться с BAM (8088/8448) и host-Traefik (80/443).
pick_http_fallback() {
  local n
  for n in $(seq 8089 8125); do
    if our_gateway_uses_host_port "${n}"; then echo "${n}"; return 0; fi
    if ! port_listening "${n}"; then echo "${n}"; return 0; fi
  done
  return 1
}

pick_https_fallback() {
  local n
  for n in $(seq 8449 8480); do
    if our_gateway_uses_host_port "${n}"; then echo "${n}"; return 0; fi
    if ! port_listening "${n}"; then echo "${n}"; return 0; fi
  done
  return 1
}

upsert_kv() {
  local file="${1:?}"
  local key="${2:?}"
  local val="${3:?}"
  if grep -qE "^[[:space:]]*${key}=" "${file}" 2>/dev/null; then
    sed -i -E "s|^[[:space:]]*${key}=.*|${key}=${val}|" "${file}"
  else
    printf '\n%s=%s\n' "${key}" "${val}" >>"${file}"
  fi
}

set -a
# shellcheck disable=SC1090
source "${ENV_FILE}"
set +a

HTTP_PORT="${GATEWAY_PUBLISH_HTTP:-80}"
HTTPS_PORT="${GATEWAY_PUBLISH_HTTPS:-443}"
CHANGED=0

if ! our_gateway_uses_host_port "${HTTP_PORT}" && port_listening "${HTTP_PORT}"; then
  NEW_HTTP="$(pick_http_fallback)" || { echo "[ensure-gateway-ports] ERROR: no free HTTP port" >&2; exit 1; }
  upsert_kv "${ENV_FILE}" "GATEWAY_PUBLISH_HTTP" "${NEW_HTTP}"
  CHANGED=1
  echo "[ensure-gateway-ports] host HTTP publish port busy → GATEWAY_PUBLISH_HTTP=${NEW_HTTP}"
  set -a
  # shellcheck disable=SC1090
  source "${ENV_FILE}"
  set +a
  HTTPS_PORT="${GATEWAY_PUBLISH_HTTPS:-443}"
fi

if ! our_gateway_uses_host_port "${HTTPS_PORT}" && port_listening "${HTTPS_PORT}"; then
  NEW_HTTPS="$(pick_https_fallback)" || { echo "[ensure-gateway-ports] ERROR: no free HTTPS port" >&2; exit 1; }
  upsert_kv "${ENV_FILE}" "GATEWAY_PUBLISH_HTTPS" "${NEW_HTTPS}"
  CHANGED=1
  echo "[ensure-gateway-ports] host HTTPS publish port busy → GATEWAY_PUBLISH_HTTPS=${NEW_HTTPS}"
fi

if [[ "${CHANGED}" -eq 1 ]]; then
  echo "[ensure-gateway-ports] updated ${ENV_FILE} (re-run compose to apply)"
fi
