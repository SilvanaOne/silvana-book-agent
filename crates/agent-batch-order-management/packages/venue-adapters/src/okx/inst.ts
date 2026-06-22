/**
 * Нормализация идентификатора рынка к OKX `instId` (spot формат BASE-QUOTE).
 * При расхождении символов агента и OKX добавьте отдельный маппер конфигом.
 */
export function toOkxSpotInstId(market: string): string {
  const m = market.trim().toUpperCase();
  if (!m.length) throw new Error("okx: empty market / instId");
  return m;
}
