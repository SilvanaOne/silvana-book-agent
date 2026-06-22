/**
 * Этап 10 (Observability): одна строка JSON на событие — для log shipping / парсинга.
 * При внедрении Prometheus используйте метки `venue`, `latency_ms`, `order_outcome` (здесь они как имена полей JSON).
 */

export type WorkerExecutionLogFields = Readonly<{
  level?: "info" | "warn" | "error";

  phase: string;
  /** `rebalance_jobs.id` — основной доменный job id в операционных трассировках (план этапа 10). */
  jobId?: string;
  bullJobId?: string;
  venue?: string;
  batchId?: string;
  orderId?: string;
  latency_ms?: number;
  order_outcome?: string;
  execProfile?: string;
  error?: string;
}>;

/** Одна строка JSON в stdout/stderr без многострочных вложений. */
export function workerExecutionJsonLog(fields: WorkerExecutionLogFields): void {
  const { level = "info", ...rest } = fields;

  const line = JSON.stringify({
    ts: new Date().toISOString(),
    component: "worker",
    level,
    ...rest,
  });

  if (level === "error") console.error(line);
  else if (level === "warn") console.warn(line);
  else console.info(line);
}
