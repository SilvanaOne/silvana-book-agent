import type { Pair } from '@arbitrage-agent/shared';

/** Bybit symbol format: concatenated base+quote (e.g. CCUSDT). */
export function pairToBybitSymbol(pair: Pair): string {
  return `${pair.base.symbol}${pair.quote.symbol}`.toUpperCase();
}
