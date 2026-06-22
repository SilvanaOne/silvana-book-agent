#!/usr/bin/env bash
# Entrypoint для sidecar-контейнера cloud-agent.
#
# Контракт:
#   - В рабочем каталоге $AGENT_DIR должны лежать agent.toml + приватный ключ
#     (артефакты ./onboard.sh, попавшие сюда через bind-mount runtime/).
#   - Команда задаётся через CLOUD_AGENT_CMD; по умолчанию "info party" — она же
#     используется в healthcheck, поэтому ничего не делает в фоне.
#   - Для Шага 3 docker-compose выставляет CLOUD_AGENT_CMD="agent --settlement-only"
#     и режим становится long-running.
#
# Использование внутри контейнера:
#   CLOUD_AGENT_CMD="info balance"           # одноразовая команда
#   CLOUD_AGENT_CMD="agent --settlement-only" # long-running
set -euo pipefail

AGENT_DIR="${AGENT_DIR:-/agent}"
BIN="${CLOUD_AGENT_BIN:-/usr/local/bin/cloud-agent}"
CMD="${CLOUD_AGENT_CMD:-info party}"

if [[ ! -x "$BIN" ]]; then
  echo "[sidecar] $BIN не найден или не исполняемый" >&2
  exit 1
fi
if [[ ! -d "$AGENT_DIR" ]]; then
  echo "[sidecar] $AGENT_DIR не существует — нужен bind-mount runtime/" >&2
  exit 1
fi
if [[ ! -f "$AGENT_DIR/agent.toml" ]]; then
  echo "[sidecar] $AGENT_DIR/agent.toml отсутствует — сначала ./onboard.sh" >&2
  exit 1
fi

cd "$AGENT_DIR"

echo "[sidecar] bin=$BIN dir=$AGENT_DIR cmd=$CMD"
# shellcheck disable=SC2086
exec "$BIN" --config agent.toml $CMD
