#!/usr/bin/env bash
# Healthcheck для контейнера cloud-agent. Возвращает 0 если cloud-agent
# может прочитать собственный agent.toml/ключ и опросить сеть Silvana
# (поэтому используется `info party` — самая дешёвая команда, не требует gas).
#
# Использование:
#   ./healthcheck.sh                 # внутри контейнера, агент в /agent
#   AGENT_DIR=... ./healthcheck.sh   # переопределить рабочий каталог
set -euo pipefail

AGENT_DIR="${AGENT_DIR:-/agent}"
BIN="${CLOUD_AGENT_BIN:-/usr/local/bin/cloud-agent}"

if [[ ! -x "$BIN" ]]; then
  echo "healthcheck: бинарь $BIN недоступен" >&2
  exit 1
fi
if [[ ! -f "$AGENT_DIR/agent.toml" ]]; then
  echo "healthcheck: $AGENT_DIR/agent.toml не найден (выполните onboard.sh)" >&2
  exit 1
fi

cd "$AGENT_DIR"
"$BIN" --config agent.toml info party >/dev/null
