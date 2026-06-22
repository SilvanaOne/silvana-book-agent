/**
 * Маппинг символов агента ({@link OrderIntent.market}) → идентификатор инструмента Canton / Temple REST.
 *
 * Продуктовые алиасы задавайте здесь явно (не смешивайте один logical `market` между Silvana и Temple без split в portfolio-engine).
 */

/** Нормализация: trim, один регистр, без лишних пробелов. */
export function normalizeTempleMarketSymbol(market: string): string {
  return market.trim().replace(/\s+/g, " ").toUpperCase();
}

/** Алиасы: ключ — нормализованный `market`; значение — id для Temple REST. */
const TEMPLE_MARKET_ALIAS: Map<string, string> = new Map([
  [normalizeTempleMarketSymbol("ETH-USDC"), "ETH/USDC"],
  [normalizeTempleMarketSymbol("BTC-USDT"), "BTC/USDT"],
]);

/** Грубый fallback: первый компонент `BASE-QUOTE`-пары через `/`. */
export function instrumentIdFallbackFromHyphenMarket(normalizedHyphenMarket: string): string {
  const parts = normalizedHyphenMarket.split("-").filter(Boolean);
  if (parts.length >= 2) {
    return `${parts[0]}/${parts[1]}`;
  }
  return normalizedHyphenMarket;
}

/** Итоговый instrument id для заявки. */
export function toTempleInstrumentId(market: string): string {
  const key = normalizeTempleMarketSymbol(market);
  return TEMPLE_MARKET_ALIAS.get(key) ?? instrumentIdFallbackFromHyphenMarket(key);
}
