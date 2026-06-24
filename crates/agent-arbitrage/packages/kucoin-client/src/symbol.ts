import type { Pair } from '@arbitrage-agent/shared';

/** KuCoin symbol format: hyphenated base-quote (e.g. CC-USDT). */
export function pairToKucoinSymbol(pair: Pair): string {
  return `${pair.base.symbol}-${pair.quote.symbol}`.toUpperCase();
}
