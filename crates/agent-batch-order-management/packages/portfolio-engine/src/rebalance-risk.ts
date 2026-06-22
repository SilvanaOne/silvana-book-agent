import type { PlannedOrder } from "./types.js";
import { D } from "./decimal-util.js";

/** Лимит номинала одной ноги (price×qty в валюте котировки). Пустой / ≤0 = выкл. */
export function parseRiskMaxOrderNotionalFromEnv(env: NodeJS.ProcessEnv = process.env) {
  const raw = env.RISK_MAX_ORDER_SIZE?.trim();
  if (!raw) return null;
  const d = D(raw);
  return d.gt(0) ? d : null;
}

/** Суммарный дневной notional по job (estimatedNotional UTC-день). Пустой / ≤0 = выкл. */
export function parseRiskMaxDailyNotionalFromEnv(env: NodeJS.ProcessEnv = process.env) {
  const raw = env.RISK_MAX_DAILY_NOTIONAL?.trim();
  if (!raw) return null;
  const d = D(raw);
  return d.gt(0) ? d : null;
}

export type RiskViolation = Readonly<{ code: string; message: string }>;

export function violationMaxOrderNotional(plannedLegIndex1Based: number, notional: unknown, limit: unknown): RiskViolation {
  return {
    code: "risk_max_order_size",
    message: `planned order leg ${plannedLegIndex1Based} notional ${String(notional)} exceeds RISK_MAX_ORDER_SIZE ${String(limit)}`,
  };
}

/** Проверка ног плана ребаланса. `limit` — из `parseRiskMaxOrderNotionalFromEnv`. */
export function checkPlannedOrdersMaxNotional(
  planned: ReadonlyArray<Pick<PlannedOrder, "price" | "qty">>,
  limit: unknown | null,
): RiskViolation | null {
  if (!limit) return null;
  for (let i = 0; i < planned.length; i++) {
    const p = planned[i]!;
    const n = D(p.price).mul(D(p.qty));
    if (n.gt(limit)) return violationMaxOrderNotional(i + 1, n, limit);
  }
  return null;
}

export function violationDailyNotional(proposed: unknown, usedToday: unknown, limit: unknown): RiskViolation {
  return {
    code: "risk_max_daily_notional",
    message: `estimated notional ${String(proposed)} plus completed live volume today ${String(usedToday)} exceeds RISK_MAX_DAILY_NOTIONAL ${String(limit)}`,
  };
}

/**
 * Дневной лимит: `usedTodayExcludingCurrent` — строка (например из Prisma `Decimal.toString()`).
 */
export function checkDailyEstimatedNotional(
  proposedEstimatedNotional: string,
  usedTodayExcludingCurrent: string,
  limit: unknown | null,
): RiskViolation | null {
  if (!limit) return null;
  const prop = D(proposedEstimatedNotional);
  if (prop.lt(0)) return null;
  const used = D(usedTodayExcludingCurrent);
  const total = used.plus(prop);
  if (total.gt(limit)) return violationDailyNotional(prop, used, limit);
  return null;
}
