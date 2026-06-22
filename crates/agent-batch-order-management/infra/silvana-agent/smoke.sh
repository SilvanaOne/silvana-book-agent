#!/usr/bin/env bash
# Smoke-runner поверх Шага 3 (Вариант А). Не торгует на цепочке:
#   1) cloud-agent info party    (диагностика: sidecar поднялся, видит ключ)
#   2) cloud-agent info balance  (диагностика: видна сеть, баланс читается)
#   3) cloud-agent buy --dry-run (диагностика: формирование RFQ-сделки и подписей
#                                  работает; реальная транзакция НЕ исполняется,
#                                  потому что cloud-agent поддерживает глобальный
#                                  флаг `--dry-run` "Dry run: prepare and verify
#                                  transaction but do not sign or execute").
#
# Запуск (sidecar Шага 3 должен быть собран; реального up не требуется — используем
# одноразовые контейнеры `docker compose run --rm`):
#
#   cd batch-order-agent/infra
#   ./silvana-agent/smoke.sh
#   ./silvana-agent/smoke.sh --market CC-USDC --amount 1.0
#
# Pre-conditions (см. updated-scenario/RUNBOOK.md):
#   - prepare.sh выполнен (есть bin/cloud-agent),
#   - onboard.sh выполнен (есть runtime/agent.toml + ключ),
#   - faucet на devnet прошёл (иначе info balance вернёт 0 — это не ошибка smoke,
#     но `buy --dry-run` может упасть на верификации без средств).
#
# Документация: task/updated-scenario/hybrid-architecture.md §3.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INFRA_DIR="$(cd "$HERE/.." && pwd)"

MARKET="${SMOKE_MARKET:-CC-USDC}"
AMOUNT="${SMOKE_AMOUNT:-1.0}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --market) MARKET="$2"; shift 2 ;;
    --amount) AMOUNT="$2"; shift 2 ;;
    -h|--help)
      sed -n '2,32p' "$0"; exit 0 ;;
    *)
      echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

if ! command -v docker >/dev/null; then
  echo "smoke: docker не установлен" >&2; exit 1
fi
if ! docker compose version >/dev/null 2>&1; then
  echo "smoke: docker compose v2 plugin не установлен" >&2; exit 1
fi

RUNTIME="$HERE/runtime"
if [[ ! -f "$RUNTIME/agent.toml" ]]; then
  echo "smoke: $RUNTIME/agent.toml отсутствует — сначала ./onboard.sh" >&2
  exit 1
fi

cd "$INFRA_DIR"

COMPOSE=(docker compose -f docker-compose.yml -f cloud-agent.compose.yml)

step() {
  local title="$1"; shift
  echo
  echo "=== smoke: $title ==="
  "${COMPOSE[@]}" run --rm \
    -e CLOUD_AGENT_CMD="$*" \
    cloud-agent
}

# Дешёвая диагностика — без --dry-run, эти команды read-only по природе.
step "info party"   info party
step "info balance" info balance

# RFQ-сделка в dry-run: cloud-agent глобальный --dry-run «prepare and verify
# transaction but do not sign or execute».
step "buy --dry-run market=$MARKET amount=$AMOUNT" \
  --dry-run buy --market "$MARKET" --amount "$AMOUNT"

echo
echo "smoke: OK"
