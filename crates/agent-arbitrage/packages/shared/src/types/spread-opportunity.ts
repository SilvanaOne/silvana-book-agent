import type { Pair } from './pair.js';
import type { DexQuote } from './quote.js';

export interface SpreadOpportunity {
  readonly pair: Pair;
  readonly buyFrom: DexQuote;
  readonly sellTo: DexQuote;
  readonly spreadPct: number;
  readonly estimatedProfitUsd: number;
  readonly detectedAt: number;
}

export const createSpreadOpportunity = (
  pair: Pair,
  buyFrom: DexQuote,
  sellTo: DexQuote,
  tradeAmountUsd: number,
): SpreadOpportunity => {
  const spreadPct = ((sellTo.sellPrice - buyFrom.buyPrice) / buyFrom.buyPrice) * 100;
  const estimatedProfitUsd = (spreadPct / 100) * tradeAmountUsd;
  return Object.freeze({
    pair,
    buyFrom,
    sellTo,
    spreadPct,
    estimatedProfitUsd,
    detectedAt: Date.now(),
  });
};
