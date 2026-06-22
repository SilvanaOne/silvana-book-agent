/**
 * Гибрид «Вариант Б» (Шаг 4): bridge между worker (BullMQ) и `cloud-agent` CLI.
 *
 * Зачем: оставить логику RFQ-такера на стороне Rust (соответствует `examples/buy_cc`
 * и README silvana-book-agent), а из TS-воркера лишь делегировать job через
 * подпроцесс. Никаких пересечений с TS-путём submitOrder / cancelOrder в книгу —
 * `cloud-agent buy|sell` сам ходит в RFQ-стрим и settle через `MulticallSettler`.
 *
 * Чистая часть (этот файл) — только конструирование команды и парсинг env.
 * Spawn вынесен в `rfq-runner.ts`, чтобы можно было unit-тестировать без сети.
 */
import type { RfqFillJobData } from "@batch-order/queue";

/**
 * Где находится `cloud-agent` относительно воркера.
 *
 * - `host`: бинарь лежит на той же машине (CI / dev), worker умеет его запускать
 *   напрямую (`spawn(binPath, …)`). Требуется `WORKER_RFQ_AGENT_DIR` (там `agent.toml`).
 * - `docker`: бинарь живёт в контейнере sidecar (см. Шаги 2 и 3); worker делает
 *   `docker exec <container> cloud-agent …`. В этом случае agent dir — путь
 *   ВНУТРИ контейнера (по умолчанию `/agent`).
 *
 * Подробности — `task/updated-scenario/hybrid-architecture.md` §6 «Вариант Б».
 */
export type RfqCliTransport =
  | Readonly<{
      kind: "host";
      /** Абсолютный путь к бинарю cloud-agent на хосте воркера. */
      binPath: string;
      /** Каталог с `agent.toml` и ключом; передаётся через `--config <dir>/agent.toml`. */
      agentDir: string;
    }>
  | Readonly<{
      kind: "docker";
      /** Имя или id контейнера со sidecar (см. cloud-agent.compose.yml: `container_name`). */
      container: string;
      /** Каталог внутри контейнера (mount target в compose файле — `/agent`). */
      agentDir: string;
    }>;

export type RfqCliConfig = Readonly<{
  transport: RfqCliTransport;
  /** Соответствует глобальному `--dry-run` cloud-agent: prepare+verify, без подписи и execute. */
  dryRunDefault: boolean;
  /** Таймаут одного запуска (мс), 0 = без лимита. */
  timeoutMs: number;
}>;

/** Безопасные дефолты при отсутствии env. */
const DEFAULT_DOCKER_CONTAINER = "bam-cloud-agent";
const DEFAULT_DOCKER_AGENT_DIR = "/agent";
const DEFAULT_TIMEOUT_MS = 15 * 60 * 1000; // 15 минут — RFQ-цикл может быть длительным

function parseBool(raw: string | undefined, fallback: boolean): boolean {
  const v = raw?.trim().toLowerCase();
  if (v == null || v.length === 0) return fallback;
  if (["1", "true", "yes", "on"].includes(v)) return true;
  if (["0", "false", "no", "off"].includes(v)) return false;
  return fallback;
}

function parseIntSafe(raw: string | undefined, fallback: number): number {
  const n = Number(raw?.trim());
  return Number.isFinite(n) && n >= 0 ? Math.floor(n) : fallback;
}

/**
 * Конфигурация bridge-а из env. Возвращает `null`, если ничего не задано — это
 * нормальное состояние: воркер тогда отвергает `rfq_fill` с понятной ошибкой.
 *
 * Env-контракт (см. `.env.example`):
 *   WORKER_RFQ_TRANSPORT       = host | docker (default: docker, если задан WORKER_RFQ_DOCKER_CONTAINER)
 *   WORKER_RFQ_BIN_PATH        = /abs/path/cloud-agent          (для host)
 *   WORKER_RFQ_AGENT_DIR       = /abs/path/runtime              (для host) или /agent (для docker)
 *   WORKER_RFQ_DOCKER_CONTAINER= bam-cloud-agent                (для docker)
 *   WORKER_RFQ_DRY_RUN_DEFAULT = 1|0  (default 1 — безопасно)
 *   WORKER_RFQ_TIMEOUT_MS      = 900000 (15 мин по умолчанию)
 */
export function rfqCliConfigFromEnv(
  env: NodeJS.ProcessEnv = process.env,
): RfqCliConfig | null {
  const transportRaw = env.WORKER_RFQ_TRANSPORT?.trim().toLowerCase();
  const dryRunDefault = parseBool(env.WORKER_RFQ_DRY_RUN_DEFAULT, true);
  const timeoutMs = parseIntSafe(env.WORKER_RFQ_TIMEOUT_MS, DEFAULT_TIMEOUT_MS);

  const explicitHost = transportRaw === "host";
  const explicitDocker = transportRaw === "docker";

  const hostBin = env.WORKER_RFQ_BIN_PATH?.trim();
  const hostDir = env.WORKER_RFQ_AGENT_DIR?.trim();
  const dockerContainer = env.WORKER_RFQ_DOCKER_CONTAINER?.trim();
  const dockerDir = env.WORKER_RFQ_AGENT_DIR?.trim();

  if (explicitHost) {
    if (!hostBin || !hostDir) return null;
    return {
      transport: { kind: "host", binPath: hostBin, agentDir: hostDir },
      dryRunDefault,
      timeoutMs,
    };
  }
  if (explicitDocker) {
    return {
      transport: {
        kind: "docker",
        container: dockerContainer ?? DEFAULT_DOCKER_CONTAINER,
        agentDir: dockerDir ?? DEFAULT_DOCKER_AGENT_DIR,
      },
      dryRunDefault,
      timeoutMs,
    };
  }

  // Автоопределение: если задан явный контейнер — docker; иначе если задан бинарь — host; иначе null.
  if (dockerContainer) {
    return {
      transport: {
        kind: "docker",
        container: dockerContainer,
        agentDir: dockerDir ?? DEFAULT_DOCKER_AGENT_DIR,
      },
      dryRunDefault,
      timeoutMs,
    };
  }
  if (hostBin && hostDir) {
    return {
      transport: { kind: "host", binPath: hostBin, agentDir: hostDir },
      dryRunDefault,
      timeoutMs,
    };
  }
  return null;
}

/** Аргументы под `cloud-agent buy|sell` без транспорта. */
function rfqSubcommandArgs(payload: RfqFillJobData): string[] {
  const args: string[] = [payload.side, "--market", payload.marketId, "--amount", payload.amount];
  if (payload.priceLimit?.trim()) args.push("--price-limit", payload.priceLimit.trim());
  if (payload.minSettlement?.trim()) args.push("--min-settlement", payload.minSettlement.trim());
  if (payload.maxSettlement?.trim()) args.push("--max-settlement", payload.maxSettlement.trim());
  if (typeof payload.intervalSec === "number" && payload.intervalSec >= 0) {
    args.push("--interval", String(Math.floor(payload.intervalSec)));
  }
  return args;
}

export type BuiltRfqCommand = Readonly<{
  cmd: string;
  args: ReadonlyArray<string>;
  /** Итоговое значение dry-run после слияния payload и cfg.dryRunDefault. */
  dryRun: boolean;
  /** Удобный плоский лог-вид «cmd arg arg …» (без shell-эскейпа — только для логов). */
  display: string;
}>;

/**
 * Сборка финальной команды для spawn:
 *
 *   host:   <binPath> --config <agentDir>/agent.toml [--dry-run] buy|sell --market … --amount …
 *   docker: docker exec -w <agentDir> <container> cloud-agent --config agent.toml [--dry-run] buy|sell …
 *
 * Глобальный флаг `--dry-run` ставится ПЕРЕД подкомандой — так его принимает CLI
 * `cloud-agent` ("Options" → "--dry-run: Dry run: prepare and verify transaction but do not sign or execute").
 */
export function buildRfqCommand(params: Readonly<{
  cfg: RfqCliConfig;
  payload: RfqFillJobData;
}>): BuiltRfqCommand {
  const { cfg, payload } = params;
  const dryRun = typeof payload.dryRun === "boolean" ? payload.dryRun : cfg.dryRunDefault;
  const sub = rfqSubcommandArgs(payload);
  const globalFlags = dryRun ? ["--dry-run"] : [];

  if (cfg.transport.kind === "host") {
    const cmd = cfg.transport.binPath;
    const configPath = `${cfg.transport.agentDir.replace(/\/$/, "")}/agent.toml`;
    const args = ["--config", configPath, ...globalFlags, ...sub];
    return { cmd, args, dryRun, display: `${cmd} ${args.join(" ")}` };
  }

  const cmd = "docker";
  const args = [
    "exec",
    "-w",
    cfg.transport.agentDir,
    cfg.transport.container,
    "cloud-agent",
    "--config",
    "agent.toml",
    ...globalFlags,
    ...sub,
  ];
  return { cmd, args, dryRun, display: `${cmd} ${args.join(" ")}` };
}

/**
 * Базовая валидация payload до запуска подпроцесса. Сообщения короткие и
 * безопасные для лога / audit-log (без приватных полей; payload может попасть
 * в JSON-лог как есть).
 */
export function validateRfqPayload(payload: RfqFillJobData): void {
  if (payload.side !== "buy" && payload.side !== "sell") {
    throw new Error(`rfq: unsupported side: ${String(payload.side)}`);
  }
  if (!payload.marketId?.trim()) throw new Error("rfq: marketId is required");
  const amt = Number(payload.amount);
  if (!Number.isFinite(amt) || amt <= 0) {
    throw new Error(`rfq: amount must be positive number: ${payload.amount}`);
  }
  for (const [k, v] of [
    ["priceLimit", payload.priceLimit],
    ["minSettlement", payload.minSettlement],
    ["maxSettlement", payload.maxSettlement],
  ] as const) {
    if (v == null) continue;
    const n = Number(v);
    if (!Number.isFinite(n) || n < 0) throw new Error(`rfq: ${k} invalid: ${v}`);
  }
  if (payload.intervalSec != null) {
    if (!Number.isFinite(payload.intervalSec) || payload.intervalSec < 0) {
      throw new Error(`rfq: intervalSec invalid: ${payload.intervalSec}`);
    }
  }
}
