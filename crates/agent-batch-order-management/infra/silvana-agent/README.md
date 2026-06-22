# Silvana cloud-agent onboarding (скелет)

Этот каталог содержит **каркас** для шага 1 из `task/updated-scenario/updated-scenario.md` —
запуск `cloud-agent onboard`. Сам инвайт от Silvana пока **не пришёл**, поэтому здесь
лежит всё, **кроме** ожидания инвайта.

Когда инвайт придёт, останется выполнить **одну** команду
(`./onboard.sh`) — она подставит ваши `EMAIL` и `INVITE_CODE`, прогонит онбординг
и сохранит артефакты в `runtime/`.

## Состав

| Файл / каталог | Назначение |
|----------------|------------|
| `bin/cloud-agent` | Локальная копия CLI (создаётся `./prepare.sh`, **не коммитится**). |
| `prepare.sh` | Копирует свежий бинарь из `task/vendor/silvana-book-agent/target/release/cloud-agent`. Можно перезапускать после новой сборки upstream. |
| `onboard.sh` | Запускает `cloud-agent onboard`. Принимает значения из env или ключи CLI. |
| `sync-to-bam-env.sh` | Переносит значения из `runtime/.env` (от cloud-agent) в `batch-order-agent/.env`. |
| `Dockerfile` | Сборка sidecar-контейнера (`batch-order-cloud-agent:dev`). Подкладывает `bin/cloud-agent` в `/usr/local/bin/` и ставит entrypoint. |
| `sidecar-entrypoint.sh` | Запуск cloud-agent в контейнере; команда задаётся `CLOUD_AGENT_CMD`. |
| `healthcheck.sh` | Использует `cloud-agent info party` как дешёвый health-пробник. |
| `smoke.sh` | Прогон: `info party` + `info balance` + `--dry-run buy …` через `docker compose run --rm`. См. RUNBOOK § Smoke-runner. |
| `.env.example` | Образец **необязательных** переменных скриптов (EMAIL, AGENT_NAME, INVITE_CODE). |
| `runtime/` | Рабочий каталог cloud-agent (`.env`, `agent.toml`, key, state) — **в .gitignore**. Монтируется в sidecar как `/agent`. |

Compose-оверлей для sidecar лежит на уровень выше — `../cloud-agent.compose.yml`.

## Порядок действий

```bash
cd batch-order-agent/infra/silvana-agent
./prepare.sh                          # 1) подкладываем cloud-agent в ./bin
# 2) дождаться инвайта от Silvana → положить в .env (см. .env.example)
./onboard.sh                          # 3) проходит онбординг → runtime/.env, runtime/agent.toml
./sync-to-bam-env.sh                  # 4) переносит RPC/PRIVATE_KEY/PARTY_AGENT в batch-order-agent/.env
```

После шага 4 — продолжать по `task/updated-scenario/RUNBOOK.md`
(faucet, `info balance`, smoke `cloud-agent buy --market CC-USDC --amount 1`, далее
sidecar — см. ниже).

## Sidecar (Шаг 2 «гибрид»)

Документ архитектуры: `task/updated-scenario/hybrid-architecture.md`.

Сборка образа и пассивный запуск (просто проверяет, что cloud-agent видит сеть и
свой `agent.toml`):

```bash
cd batch-order-agent/infra
docker compose \
  -f docker-compose.yml \
  -f cloud-agent.compose.yml \
  up -d --build cloud-agent

docker compose -f docker-compose.yml -f cloud-agent.compose.yml logs -f cloud-agent
```

В Шаге 2 контейнер выполняет `info party` и завершается (или зависает в `--no-restart`
зависимости от вашей конфигурации). Это намеренно: long-running режим
`--settlement-only` вводится только в **Шаге 3 (вариант А)** — там же `restart`
меняется на `unless-stopped`.

Чтобы выполнить разовую команду cloud-agent с теми же mount-ами (например `info balance`):

```bash
docker compose -f docker-compose.yml -f cloud-agent.compose.yml run --rm \
  -e CLOUD_AGENT_CMD="info balance" cloud-agent
```

## Вариант Б: job `rfq_fill` через CLI (Шаг 4)

TS-воркер умеет делегировать RFQ-такер `cloud-agent buy|sell` per-job. Никаких
изменений тут не требуется: в воркере появляется тип job `rfq_fill`, а bridge
(`apps/worker/src/silvana/rfq-cli.ts`) делает `docker exec` в этот контейнер.

Минимальная конфигурация в `batch-order-agent/.env`:

```env
WORKER_RFQ_DOCKER_CONTAINER=bam-cloud-agent
WORKER_RFQ_AGENT_DIR=/agent
WORKER_RFQ_DRY_RUN_DEFAULT=1
```

Подробнее — в `task/updated-scenario/RUNBOOK.md` (раздел про Вариант Б) и
`task/updated-scenario/hybrid-architecture.md` §7.

## Что НЕ делает этот скелет

- Не выпускает за вас инвайт.
- Не запускает `cloud-agent agent` в долгоживущем режиме (это решение шага 2 из
  обзора готовности — гибрид vs Rust-перевод).
- Не подменяет JWT в TS-стеке `batch-order-agent` — на данный момент TS требует
  статический `SILVANA_JWT`, а cloud-agent выпускает JWT динамически из приватного
  ключа. Это известный архитектурный gap (см. ответ ассистента в чате /
  `task/updated-scenario/` обзор готовности).

## Безопасность

- Никаких секретов в коммитах: всё, что пишет `cloud-agent` (private key, party id,
  agent.toml), попадает в `runtime/` — он в `.gitignore`.
- Перед `./onboard.sh` убедитесь, что `.env` с `INVITE_CODE` и `EMAIL` тоже
  не закоммичен (он попадает под `.env` правило в `.gitignore`).
