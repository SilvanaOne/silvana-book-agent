import type { OrderIntent } from "@batch-order/venue-adapters";

export type RiskContext = Readonly<{ maxPerVenueUsd?: Record<string, string> }>;

/** Пер-venue потолки до VenueAdapter (`okx`, `binance-otc`, …); интеграция с RISK_* из env — Этап 4. */
export function passesRiskRules(
  intent: OrderIntent,
  venue: string,
  ctx: RiskContext,
): { ok: true } | { ok: false; code: string; message: string } {
  const cap = ctx.maxPerVenueUsd?.[venue];
  if (cap === undefined) return { ok: true };

  const priceRaw = intent.price?.trim();

  /** Без явной цены лимита оценную нотацию проверять нельзя — пропускаем до полноценной интеграции. */
  if (intent.type !== "limit" || priceRaw === undefined || priceRaw.length === 0) {
    return { ok: true };
  }

  const notional = Number(priceRaw) * Number(intent.qty.trim());
  const maxUsd = Number(cap.trim());

  if (!Number.isFinite(notional) || !Number.isFinite(maxUsd)) {
    return { ok: true };
  }

  if (notional > maxUsd) {
    return {
      ok: false,
      code: "per_venue_notional_cap",
      message: `estimated notional ${notional} exceeds cap ${maxUsd} for venue ${venue}`,
    };
  }

  return { ok: true };
}
