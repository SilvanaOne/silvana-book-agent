/** Режим отправки ордеров в Silvana Book для worker (`rpc` нужен JWT). */
export type WorkerSilvanaMode = "rpc" | "plan_only";

/** `rpc` — реальные `submitOrder/cancelOrder` при наличии `SILVANA_JWT`; иначе — только локальный план. */
export function workerSilvanaModeFromEnv(env: NodeJS.ProcessEnv = process.env): WorkerSilvanaMode {

  const override = env.WORKER_SILVANA_EXECUTION?.trim().toLowerCase();

  if (override === "plan_only") return "plan_only";

  if (override === "rpc") {
    const jwtOk = Boolean(env.SILVANA_JWT?.trim());

    return jwtOk ? "rpc" : "plan_only";
  }


  const jwt = env.SILVANA_JWT?.trim();

  return jwt ? "rpc" : "plan_only";

}

/**
 * Гибрид «вариант А» (Шаг 3): рядом с воркером работает long-running
 * `cloud-agent agent --settlement-only`. Воркер сам ничего не settle-ит —
 * только декларирует факт sidecar в логах. Поведение path-а исполнения
 * (submit / plan_only) не меняется: это лишь индикатор для оператора.
 *
 * Контракт значения `WORKER_SILVANA_SETTLEMENT_SIDECAR`:
 *   1 / true / yes / on — включить лог-баннер про sidecar (поведение прежнее);
 *   иначе — баннер не печатать.
 *
 * См. `task/updated-scenario/hybrid-architecture.md` §3 и §6.
 */
export function workerSettlementSidecarEnabledFromEnv(
  env: NodeJS.ProcessEnv = process.env,
): boolean {
  const raw = env.WORKER_SILVANA_SETTLEMENT_SIDECAR?.trim().toLowerCase();
  if (!raw) return false;
  return raw === "1" || raw === "true" || raw === "yes" || raw === "on";
}

