import type { VenueName } from "@batch-order/venue-adapters";
import type { RouterConfig } from "./routeOrder.js";

const KNOWN_DEFAULTS = new Set<string>(["silvana", "okx", "temple", "okx-liquid", "binance-otc"]);

function parseVenue(raw: string | undefined, fallback: VenueName): VenueName {
  const s = raw?.trim().toLowerCase();
  if (!s || !KNOWN_DEFAULTS.has(s)) return fallback;
  return s as VenueName;
}

/** Env для последующего `chooseVenue` (полное подключение в воркере — Этап 4). */
export function routerConfigFromEnv(env: Readonly<NodeJS.ProcessEnv> = process.env): RouterConfig {
  const legacyWorker = env.WORKER_DEFAULT_EXECUTION_VENUE;
  const primary = env.ROUTER_DEFAULT_VENUE;
  const rawDefault =
    typeof primary === "string" && primary.trim().length > 0
      ? primary
      : typeof legacyWorker === "string" && legacyWorker.trim().length > 0
        ? legacyWorker
        : undefined;

  const defaultVenue = parseVenue(rawDefault, "silvana");

  const minQtyRfQ =
    typeof env.ROUTER_MIN_QTY_RFQ === "string" && env.ROUTER_MIN_QTY_RFQ.trim().length > 0
      ? env.ROUTER_MIN_QTY_RFQ.trim()
      : "0";

  const maxQtyOkx =
    typeof env.ROUTER_MAX_ORDER_SIZE_OKX === "string" && env.ROUTER_MAX_ORDER_SIZE_OKX.trim().length > 0
      ? env.ROUTER_MAX_ORDER_SIZE_OKX.trim()
      : "50000";

  const maxQtyLiquidRaw = env.ROUTER_MAX_ORDER_SIZE_LIQUID?.trim();

  return {
    defaultVenue,
    minQtyRfQ,
    maxQtyOkx,
    ...(maxQtyLiquidRaw?.length ? { maxQtyLiquid: maxQtyLiquidRaw } : {}),
  };
}
