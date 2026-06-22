#!/usr/bin/env bash
# Переносит значения из ./runtime/.env (cloud-agent) в batch-order-agent/.env.
# Источник правил соответствия:
#   - README upstream: `silvana-book-agent` секция «Configuration»
#   - task/docs/phase-1-onboarding-and-sdk.md §2.3
#
# Маппинг (минимальный):
#   ORDERBOOK_GRPC_URL      → SILVANA_RPC
#   PARTY_AGENT_PRIVATE_KEY → SILVANA_PRIVATE_KEY
#   PARTY_AGENT             → SILVANA_PARTY_ID (если используется в вашем стеке)
#
# SILVANA_JWT преднамеренно НЕ заполняется: cloud-agent выпускает JWT
# динамически из Ed25519 key. TS-стек batch-order-agent на сегодняшний день
# ожидает статический JWT — это известный архитектурный gap (см. обзор готовности
# в чате / task/updated-scenario/).
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BAM_ROOT="$(cd "$HERE/../.." && pwd)"
SRC="$HERE/runtime/.env"
DST="$BAM_ROOT/.env"

if [[ ! -f "$SRC" ]]; then
  echo "нет $SRC — сначала ./onboard.sh" >&2
  exit 1
fi
if [[ ! -f "$DST" ]]; then
  echo "нет $DST — скопируйте $BAM_ROOT/.env.example в $DST и повторите" >&2
  exit 1
fi

get() {
  local key="$1"
  awk -F= -v K="$key" '$1==K { sub(/^[^=]*=/, ""); print; exit }' "$SRC"
}
set_kv() {
  local key="$1" val="$2" file="$3"
  if grep -qE "^${key}=" "$file"; then
    # macOS sed-совместимый in-place: пишем во временный.
    awk -v K="$key" -v V="$val" 'BEGIN{FS=OFS="="} $1==K { $0=K"="V } { print }' "$file" >"$file.tmp"
    mv "$file.tmp" "$file"
  else
    printf '%s=%s\n' "$key" "$val" >>"$file"
  fi
}

rpc="$(get ORDERBOOK_GRPC_URL || true)"
pk="$(get PARTY_AGENT_PRIVATE_KEY || true)"
party="$(get PARTY_AGENT || true)"

[[ -n "$rpc"   ]] && set_kv SILVANA_RPC          "$rpc"   "$DST" && echo "set SILVANA_RPC"
[[ -n "$pk"    ]] && set_kv SILVANA_PRIVATE_KEY  "$pk"    "$DST" && echo "set SILVANA_PRIVATE_KEY"
[[ -n "$party" ]] && set_kv SILVANA_PARTY_ID     "$party" "$DST" && echo "set SILVANA_PARTY_ID"

echo "напоминание: SILVANA_JWT не заполнен — см. обзор готовности про TS↔Rust gap."
