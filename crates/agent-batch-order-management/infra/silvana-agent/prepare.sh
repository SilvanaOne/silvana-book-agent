#!/usr/bin/env bash
# Подкладывает свежий cloud-agent в ./bin/.
# Источник: task/vendor/silvana-book-agent/target/release/cloud-agent
# (сборка задокументирована в task/updated-scenario/RUNBOOK.md, секция «Вариант B»).
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$HERE/../../.." && pwd)"
SRC="$REPO_ROOT/task/vendor/silvana-book-agent/target/release/cloud-agent"
DST="$HERE/bin/cloud-agent"

if [[ ! -x "$SRC" ]]; then
  echo "cloud-agent не найден: $SRC" >&2
  echo "Соберите его командой (см. task/updated-scenario/RUNBOOK.md):" >&2
  echo "  cd task/vendor/silvana-book-agent && \\" >&2
  echo "  PROTOC=\"\$PWD/../protoc/bin/protoc\" cargo build --release -p cli" >&2
  exit 1
fi

mkdir -p "$HERE/bin"
install -m 0755 "$SRC" "$DST"
echo "ok: $DST"
"$DST" --help >/dev/null
echo "binary works"
