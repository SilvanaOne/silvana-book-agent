# batch-order-agent

Монорепозиторий **Batch Order Management** на Silvana (`npm` workspaces + Turborepo).

Подробнее в плане: `../task/implementation-plan.md`, ТЗ `../task/bam_task.txt`, фазы 0–1 `../task/docs/`.

## Состав

| Путь | Назначение |
|------|------------|
| `apps/api` | HTTP API (Express), каркас маршрутов ребаланса |
| `apps/worker` | Long-running процесс: Silvana sanity + heartbeat (полный executor — фаза 6) |
| `apps/web` | Next.js dashboard (минимальный UI) |
| `packages/shared` | Доменные типы и константы |
| `packages/silvana-client` | Обёртка над `@silvana-one/orderbook` + конфиг из env |
| `packages/portfolio-engine` | Расчёт портфеля (фаза 4) |
| `packages/db` | Prisma client + экспорт `@batch-order/db` |
| `prisma/` | schema (bootstrap-модель; полная модель из ТЗ — фаза 3) |
| `infra/docker-compose.yml` | PostgreSQL + Redis для локальной разработки |
| `infra/silvana-agent/` | Каркас онбординга `cloud-agent` + sidecar (`Dockerfile`, `smoke.sh`). Гибрид с Silvana — `../task/updated-scenario/hybrid-architecture.md` |
| `infra/cloud-agent.compose.yml` | Compose-оверлей: пассивный sidecar (Шаг 2). |
| `infra/cloud-agent-variant-a.compose.yml` | Compose-оверлей: long-running `agent --settlement-only` (Шаг 3, Вариант А). |
| `deploy/` | Продакшен: Docker Compose, nginx, **[деплой на bamagent.spcatcher.cfd](deploy/DEPLOY-BAMAGENT.md)** (`46.250.253.67`), `deploy-to-server.sh` |

## Продакшен (Docker, VPS)

Текущий сервер **`46.250.253.67`**, публичный адрес **`https://bamagent.spcatcher.cfd`** (SSH по ключу, без пароля). Полная процедура: **`deploy/DEPLOY-BAMAGENT.md`**.

> Текущий снимок состояния прода (что подтверждено работающим, какие блокеры
> Silvana остаются, как тестировать через UI без живой Silvana) — см.
> **`../task/docs/status-2026-05-22.md`**.

После копирования и правки `deploy/.env.stack` (пример: `deploy/.env.stack.bamagent.example`):

```bash
export UPLOAD_ENV_STACK=1
./deploy/deploy-to-server.sh --force
```

## Быстрый старт

Разовый сценарий «поднять Postgres/Redis → миграции → seed → сборка → `turbo dev` (api + web + worker)»:

```bash
cd batch-order-agent
./run.sh --help           # опции: --prepare-only, --no-seed, --skip-infra, --with-infra
./run.sh
```

`./run.sh` без флагов теперь **авто-определяет**, поднята ли инфра: если `postgres`+`redis` уже бегут в compose-проекте `infra/` — `docker compose up` пропускается. Форсировать пропуск — `--skip-infra`, форсировать запуск (или пересоздание) — `--with-infra`.

Перед `exec npm run dev` скрипт также **подгружает корневой `.env`** через `set -a; source .env; set +a`, чтобы Next dev увидел `NEXT_PUBLIC_*` без отдельного `apps/web/.env`. Список ключевых переменных:

- `DEMO_TOOLS=1` — серверный гейт для `POST /api/portfolio/:id/imitate-drift` и (TODO) `rebalance-now`. Без него endpoint вернёт 404.
- `NEXT_PUBLIC_DEMO_TOOLS=1` — клиентский гейт для блока **Manual Drift Imitator** на `/rebalance`. Должен совпадать с `DEMO_TOOLS`.
- `NEXT_PUBLIC_SILVANA_SCAN_URL=https://silvascan.io/devnet/tx/{hash}` — шаблон ссылки скрепки в Rebalance-модалке. Должен содержать `{hash}` placeholder.

На проде те же переменные живут в `deploy/.env.stack` и запекаются в Docker-образ web (см. `Dockerfile.web` `ARG NEXT_PUBLIC_*` + `docker-compose.yml` `build.args`).

Ручная расстановка (эквивалент первой части `run.sh`):

```bash
cd batch-order-agent
cp .env.example .env # заполните по необходимости
npm install
npm run build
```

Запуск отдельных приложений без параллельного `turbo dev` всего монорепо:

```bash
npm run dev:api     # Express :3000 (см. apps/api)
npm run dev:web     # Next :3001 — задайте NEXT_PUBLIC_API_URL в .env корня если API не на localhost:3000
npm run dev:worker  # Worker (Silvana pricing sanity при старте)
```

БД (фаза 3+):

```bash
docker compose -f infra/docker-compose.yml up -d
# контейнер пробрасывает Postgres на хост-порт 5433 (чтобы не конфликтовать с локальным 5432)

# в .env используйте DATABASE_URL с портом :5433 как в .env.example
npm run db:migrate:apply   # prisma migrate deploy + generate

npm run db:seed         # демо-портфель (требует актуального Prisma Client — после generate)
```

См. `prisma/schema.prisma` и `prisma/migrations/` — полная схема из ТЗ + сид.

Смоук Silvana (копия задачи из `task/scripts/smoke-orderbook`, путь `.env` — корень **этого** репозитория):

```bash
cd scripts/smoke-orderbook && npm install && npm run smoke
```

## Гибрид с `cloud-agent` (sidecar Rust)

Подробности: **`../task/updated-scenario/hybrid-architecture.md`** (карта
ответственности TS↔Rust, Варианты А и Б) и **`../task/updated-scenario/RUNBOOK.md`**
(команды).

Краткий «крючок» для оператора после онбординга:

```bash
cd infra
# Шаг 2 (пассивный sidecar, info party):
docker compose -f docker-compose.yml -f cloud-agent.compose.yml up -d --build cloud-agent

# Шаг 3 (Вариант А — long-running `agent --settlement-only`):
docker compose -f docker-compose.yml -f cloud-agent.compose.yml -f cloud-agent-variant-a.compose.yml up -d --build cloud-agent

# Smoke поверх Шага 3 (без реальных транзакций):
./silvana-agent/smoke.sh

# Вариант Б — TS-job `rfq_fill` через CLI bridge:
# в .env корня: WORKER_RFQ_DOCKER_CONTAINER=bam-cloud-agent + WORKER_RFQ_DRY_RUN_DEFAULT=1
```

## Префикс пакетов

Scoped workspace: **`@batch-order/*`**.

## Sync в upstream `SilvanaOne/silvana-book-agent`

Этот монорепо — **источник истины** для агента `batch-order-management`.
Опубликованная snapshot-копия лежит в `SilvanaOne/silvana-book-agent` на
ветке `new-agents`, в пути `crates/agent-batch-order-management/`.

История разработки живёт **здесь**; upstream получает одиночные «Sync»-коммиты
после каждой нашей публикации. Скрипт-обёртка в корне monorepo:

```bash
# из корня monorepo (на уровень выше batch-order-agent/)
./scripts/sync-to-silvana.sh "<one-line summary of what changed>"
```

Что делает:
1. `git clone --depth 1 --branch new-agents` upstream во временный каталог.
2. `rsync --delete` `batch-order-agent/` → `crates/agent-batch-order-management/`,
   исключая `node_modules/`, `.next/`, `dist/`, `*.tsbuildinfo`, `.env*`,
   `coverage/`, `infra/silvana-agent/runtime/`, `infra/silvana-agent/bin/`.
3. `git commit` с сообщением `Sync agent-batch-order-management: <summary>`
   + указание нашего `remote + SHA` для traceability.
4. `git push origin new-agents`.
5. Удаляет временный clone.

Сухой прогон без push: `./scripts/sync-to-silvana.sh --dry-run`.

Override адреса/ветки/папки:

```bash
UPSTREAM_REPO="git@github.com:SilvanaOne/silvana-book-agent.git" \
UPSTREAM_BRANCH="new-agents" \
UPSTREAM_PATH="crates/agent-batch-order-management" \
./scripts/sync-to-silvana.sh "..."
```

Когда запускать:
- После каждого `git push` в наш origin, если хотим, чтобы upstream был в курсе.
- Не запускать после каждого мелкого WIP-коммита — это создаёт шум в upstream-логе.
  Лучше копить несколько коммитов и публиковать одним «Sync» с осмысленным summary.

Один деплой = одно «Sync …» в upstream. Полный лог изменений остаётся здесь.
