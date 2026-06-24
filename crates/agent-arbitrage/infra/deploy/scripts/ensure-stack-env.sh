#!/usr/bin/env bash
# Если на сервере ещё нет infra/deploy/.env.stack, копирует пример один раз.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INFRA_DEPLOY="$(cd "${SCRIPT_DIR}/.." && pwd)"

ENV_TARGET="${INFRA_DEPLOY}/.env.stack"
ENV_SRC="${INFRA_DEPLOY}/.env.stack.arbitrage.example"

if [[ -f "${ENV_TARGET}" ]]; then
  exit 0
fi

if [[ ! -f "${ENV_SRC}" ]]; then
  echo "ERROR: missing ${ENV_SRC}" >&2
  exit 1
fi

cp "${ENV_SRC}" "${ENV_TARGET}"
echo "[ensure-stack-env] Created ${ENV_TARGET}. Review before production traffic."
