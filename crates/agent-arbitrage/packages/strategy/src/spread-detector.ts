/**
 * Spread detection — pure functional core.
 *
 * Adapted from dex-arbitrager `src/usecases/AnalyzeSpreadsUseCase.ts`. No side
 * effects: takes quotes + config, returns sorted list of opportunities.
 *
 * This module lives in open-core. Trade execution lives in the proprietary
 * executor (separate repo) — see md_docs/12-component-split-open-vs-proprietary.md.
 */

import {
  type Pair,
  type DexQuote,
  type SpreadOpportunity,
  createSpreadOpportunity,
  tokenPairKey,
} from '@arbitrage-agent/shared';
import type { TradeConfig } from './types.js';

/**
 * Find all profitable spread opportunities for a single token pair from a set
 * of quotes across multiple venues. Returns frozen, descending by spreadPct.
 */
export function findSpreads(
  pair: Pair,
  quotes: readonly DexQuote[],
  config: TradeConfig,
): readonly SpreadOpportunity[] {
  if (quotes.length < 2) return Object.freeze([]);

  const opportunities: SpreadOpportunity[] = [];

  for (let i = 0; i < quotes.length; i++) {
    for (let j = 0; j < quotes.length; j++) {
      if (i === j) continue;
      const buyFrom = quotes[i]!;
      const sellTo = quotes[j]!;
      if (buyFrom.buyPrice <= 0 || sellTo.sellPrice <= 0) continue;
      if (buyFrom.buyPrice >= sellTo.sellPrice) continue;

      const spreadPct = ((sellTo.sellPrice - buyFrom.buyPrice) / buyFrom.buyPrice) * 100;
      if (spreadPct < config.targetSpreadPercent) continue;
      if (config.maxSpreadPercent > 0 && spreadPct > config.maxSpreadPercent) continue;

      opportunities.push(createSpreadOpportunity(pair, buyFrom, sellTo, config.tradeSizeUsd));
    }
  }

  return Object.freeze(opportunities.sort((a, b) => b.spreadPct - a.spreadPct));
}

/** Single best opportunity for a pair, or null. */
export function findBestSpread(
  pair: Pair,
  quotes: readonly DexQuote[],
  config: TradeConfig,
): SpreadOpportunity | null {
  const spreads = findSpreads(pair, quotes, config);
  return spreads[0] ?? null;
}

/**
 * Analyse multiple pairs at once: group by canonical token-pair key (e.g. CC/USDT)
 * so quotes from different venues for the same underlying pair compete.
 */
export function analyzeAllPairs(
  pairQuotes: ReadonlyMap<Pair, readonly DexQuote[]>,
  config: TradeConfig,
): readonly SpreadOpportunity[] {
  const byTokenPair = new Map<string, { representative: Pair; quotes: DexQuote[] }>();

  for (const [pair, quotes] of pairQuotes) {
    const key = tokenPairKey(pair);
    const entry = byTokenPair.get(key);
    if (entry) {
      entry.quotes.push(...quotes);
    } else {
      byTokenPair.set(key, { representative: pair, quotes: [...quotes] });
    }
  }

  const allOpportunities: SpreadOpportunity[] = [];
  for (const { representative, quotes } of byTokenPair.values()) {
    allOpportunities.push(...findSpreads(representative, quotes, config));
  }

  return Object.freeze(allOpportunities.sort((a, b) => b.spreadPct - a.spreadPct));
}
