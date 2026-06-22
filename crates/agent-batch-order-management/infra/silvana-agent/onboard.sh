#!/usr/bin/env bash
# Проводит cloud-agent onboard с параметрами из .env (этого каталога) или CLI.
# Рабочие артефакты пишет в ./runtime/{.env,agent.toml,...}.
#
# Использование:
#   ./onboard.sh                    # читает все параметры из ./.env
#   ./onboard.sh --invite-code XXX  # переопределение по ключам cloud-agent
#
# Требует:
#   1) ./prepare.sh выполнен (есть ./bin/cloud-agent).
#   2) В ./.env заданы SILVANA_INVITE_CODE и SILVANA_ONBOARD_EMAIL.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN="$HERE/bin/cloud-agent"
ENVFILE="$HERE/.env"
RUNTIME="$HERE/runtime"

if [[ ! -x "$BIN" ]]; then
  echo "не найден $BIN — запустите ./prepare.sh" >&2
  exit 1
fi

# shellcheck disable=SC1090
[[ -f "$ENVFILE" ]] && set -a && . "$ENVFILE" && set +a

: "${SILVANA_AGENT_NAME:=batch-order-management}"
: "${SILVANA_RPC:=https://orderbook-devnet.silvana.dev:443}"

if [[ -z "${SILVANA_INVITE_CODE:-}" ]]; then
  echo "SILVANA_INVITE_CODE не задан (положите в $ENVFILE или передайте --invite-code)" >&2
  exit 1
fi
if [[ -z "${SILVANA_ONBOARD_EMAIL:-}" ]]; then
  echo "SILVANA_ONBOARD_EMAIL не задан (положите в $ENVFILE)" >&2
  exit 1
fi

mkdir -p "$RUNTIME"
cd "$RUNTIME"

echo "[onboard] runtime: $RUNTIME"
echo "[onboard] rpc:     $SILVANA_RPC"
echo "[onboard] agent:   $SILVANA_AGENT_NAME"
echo "[onboard] email:   $SILVANA_ONBOARD_EMAIL"
echo "[onboard] starting cloud-agent onboard ..."

"$BIN" onboard \
  --rpc "$SILVANA_RPC" \
  --agent-name "$SILVANA_AGENT_NAME" \
  --email "$SILVANA_ONBOARD_EMAIL" \
  --invite-code "$SILVANA_INVITE_CODE" \
  "$@"

echo "[onboard] done — артефакты в $RUNTIME"
ls -1 "$RUNTIME"
