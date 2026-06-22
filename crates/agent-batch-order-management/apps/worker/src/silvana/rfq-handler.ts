/**
 * Шаг 4 / Вариант Б: обработчик job `rfq_fill`. Пишет audit-log с фазами
 * `requested` → (`completed` | `failed`). НЕ создаёт Order/OrderBatch в БД —
 * RFQ-такер `cloud-agent buy|sell` сам пишет в книгу через свои стримы; TS
 * увидит результат как обычные order-события (`subscribeOrders`), которые уже
 * подхватываются `stream-consumer.ts`.
 */
import { appendAuditLog as defaultAppendAuditLog } from "@batch-order/db";
import type { Prisma } from "@batch-order/db";
import type { RfqFillJobData } from "@batch-order/queue";

import { workerExecutionJsonLog } from "../lib/execution-observability.js";
import {
  buildRfqCommand,
  rfqCliConfigFromEnv,
  validateRfqPayload,
  type RfqCliConfig,
} from "./rfq-cli.js";
import { defaultRfqExecutor, type RfqExecutor, type RfqRunResult } from "./rfq-runner.js";

/** Узкий контракт audit-log для тестируемости (соответствует сигнатуре @batch-order/db). */
export type AuditLogWriter = (params: Readonly<{
  actorId: string;
  action: string;
  entityType: string;
  entityId: string;
  payload: Prisma.InputJsonValue;
}>) => Promise<unknown>;

/** Усечение длинных строк для audit-log (Postgres jsonb всё равно сожмёт, но не раздуваем). */
const AUDIT_TAIL_LIMIT = 4_000;

function tail(s: string, max = AUDIT_TAIL_LIMIT): string {
  if (s.length <= max) return s;
  return s.slice(s.length - max);
}

function asJson<T>(value: T): Prisma.InputJsonValue {
  return JSON.parse(JSON.stringify(value)) as Prisma.InputJsonValue;
}

export type RfqFillDeps = Readonly<{
  configFromEnv?: () => RfqCliConfig | null;
  exec?: RfqExecutor;
  appendAuditLog?: AuditLogWriter;
  /** Идентификатор bull-job для трассировок. */
  bullJobId?: string;
}>;

/**
 * Контракт: при `cfg == null` job сразу `fails` с понятной причиной, чтобы
 * оператор увидел отсутствующую env-конфигурацию. При `success=false` от
 * cloud-agent (ненулевой exit) — `fails`, чтобы BullMQ применил backoff.
 *
 * При `dryRun=true` сохраняемся success даже если exit != 0? — нет. Глобальный
 * `--dry-run` cloud-agent должен возвращать 0 при успешной верификации; любая
 * ошибка верификации — это легитимный fail.
 */
export async function handleRfqFill(
  payload: RfqFillJobData,
  deps: RfqFillDeps = {},
): Promise<void> {
  const configFromEnv = deps.configFromEnv ?? rfqCliConfigFromEnv;
  const exec = deps.exec ?? defaultRfqExecutor;
  const appendAuditLog = deps.appendAuditLog ?? defaultAppendAuditLog;

  validateRfqPayload(payload);

  const cfg = configFromEnv();
  if (!cfg) {
    workerExecutionJsonLog({
      level: "error",
      phase: "rfq.fill.misconfigured",
      bullJobId: deps.bullJobId,
      venue: "silvana",
      error: "WORKER_RFQ_TRANSPORT/WORKER_RFQ_DOCKER_CONTAINER/WORKER_RFQ_BIN_PATH not set",
    });
    await appendAuditLog({
      actorId: "worker",
      action: "rfq.fill.misconfigured",
      entityType: "rfq_fill",
      entityId: payload.clientRef?.trim() || `${payload.side}:${payload.marketId}:${payload.amount}`,
      payload: asJson({ payload, reason: "rfq_cli_not_configured" }),
    });
    throw new Error("rfq_fill: cloud-agent CLI bridge не сконфигурирован (см. WORKER_RFQ_* в .env)");
  }

  const built = buildRfqCommand({ cfg, payload });
  const entityId = payload.clientRef?.trim() || `${payload.side}:${payload.marketId}:${payload.amount}`;

  workerExecutionJsonLog({
    phase: "rfq.fill.dispatch",
    bullJobId: deps.bullJobId,
    venue: "silvana",
    execProfile: built.dryRun ? "rfq.dry_run" : "rfq",
  });

  await appendAuditLog({
    actorId: "worker",
    action: "rfq.fill.requested",
    entityType: "rfq_fill",
    entityId,
    payload: asJson({
      payload,
      transport: cfg.transport.kind,
      dryRun: built.dryRun,
      command: built.display,
    }),
  });

  let result: RfqRunResult;
  const t0 = Date.now();
  try {
    result = await exec({ cfg, built });
  } catch (err: unknown) {
    const message = err instanceof Error ? err.message : String(err);
    workerExecutionJsonLog({
      level: "error",
      phase: "rfq.fill.spawn_error",
      bullJobId: deps.bullJobId,
      venue: "silvana",
      error: message,
      latency_ms: Date.now() - t0,
    });
    await appendAuditLog({
      actorId: "worker",
      action: "rfq.fill.failed",
      entityType: "rfq_fill",
      entityId,
      payload: asJson({ payload, dryRun: built.dryRun, error: message }),
    });
    throw err instanceof Error ? err : new Error(message);
  }

  const latency = Date.now() - t0;

  if (result.exitCode !== 0 || result.timedOut) {
    workerExecutionJsonLog({
      level: "error",
      phase: result.timedOut ? "rfq.fill.timeout" : "rfq.fill.failed",
      bullJobId: deps.bullJobId,
      venue: "silvana",
      latency_ms: latency,
      error: tail(result.stderr, 512) || `exit ${result.exitCode}`,
    });
    await appendAuditLog({
      actorId: "worker",
      action: result.timedOut ? "rfq.fill.timeout" : "rfq.fill.failed",
      entityType: "rfq_fill",
      entityId,
      payload: asJson({
        payload,
        dryRun: built.dryRun,
        exitCode: result.exitCode,
        timedOut: result.timedOut,
        stdoutTail: tail(result.stdout),
        stderrTail: tail(result.stderr),
        latencyMs: latency,
      }),
    });
    throw new Error(
      result.timedOut
        ? `rfq_fill: cloud-agent timed out after ${cfg.timeoutMs}ms`
        : `rfq_fill: cloud-agent exit=${result.exitCode}`,
    );
  }

  workerExecutionJsonLog({
    phase: built.dryRun ? "rfq.fill.dry_run_completed" : "rfq.fill.completed",
    bullJobId: deps.bullJobId,
    venue: "silvana",
    latency_ms: latency,
    order_outcome: built.dryRun ? "verified" : "filled_or_pending",
  });
  await appendAuditLog({
    actorId: "worker",
    action: built.dryRun ? "rfq.fill.dry_run_completed" : "rfq.fill.completed",
    entityType: "rfq_fill",
    entityId,
    payload: asJson({
      payload,
      dryRun: built.dryRun,
      latencyMs: latency,
      stdoutTail: tail(result.stdout),
    }),
  });
}
