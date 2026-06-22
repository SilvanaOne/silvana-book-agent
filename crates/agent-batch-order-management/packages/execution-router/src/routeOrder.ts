import type { OrderIntent, VenueName } from "@batch-order/venue-adapters";

export type RouterConfig = Readonly<{
  defaultVenue: VenueName;
  /** Зарезервировано под политики OKX REST (Этап 5); не используется в `chooseVenue` Этапа 2. */
  maxQtyOkx: string;
  /** Минимальный qty для сравнения с `intent.qty` при маршрутизации в OKX Liquid. */
  minQtyRfQ: string;
  /**
   * Верхний предел heuristic-маршрутизации по числовому `qty` на OKX Liquid (Этап 7).
   * Не ограничивает явный `execProfile`: `rfq` | `block` | `otc`.
   * Если не задан — потолка нет (кроме `minQtyRfQ`).
   */
  maxQtyLiquid?: string;
}>;

export function chooseVenue(intent: OrderIntent, cfg: RouterConfig): VenueName {
  if (intent.venue) return intent.venue;

  if (intent.execProfile === "rfq" || intent.execProfile === "block") {
    return "okx-liquid";
  }

  if (intent.execProfile === "otc") {
    return "binance-otc";
  }

  if (intent.type === "market") {
    return "okx";
  }

  const qtyNorm = intent.qty.trim();

  if (
    /^[0-9]+(\.[0-9]+)?$/.test(qtyNorm) &&
    Number(qtyNorm) >= Number(cfg.minQtyRfQ.trim())
  ) {
    const maxL = cfg.maxQtyLiquid?.trim();
    if (maxL !== undefined && maxL.length > 0) {
      const cap = Number(maxL);
      if (Number.isFinite(cap) && Number(qtyNorm) <= cap) {
        return "okx-liquid";
      }
    } else {
      return "okx-liquid";
    }
  }

  if (/canton/i.test(intent.market)) {
    return "temple";
  }

  return cfg.defaultVenue;
}
