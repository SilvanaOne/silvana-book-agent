import type { Pair, DexQuote, SpreadOpportunity } from '@arbitrage-agent/shared';

export interface TradeConfig {
  /** Minimum spread percentage to consider an opportunity (e.g. 0.25 = 25 bps). */
  readonly targetSpreadPercent: number;
  /** Maximum spread percentage — anything above is treated as bogus quote (e.g. 10 = 10%). 0 disables cap. */
  readonly maxSpreadPercent: number;
  /** Trade size in USD used for profit estimation. */
  readonly tradeSizeUsd: number;
}

export interface ScanCycleResult {
  readonly quoteMap: ReadonlyMap<Pair, readonly DexQuote[]>;
  readonly opportunities: readonly SpreadOpportunity[];
  readonly fetchErrors: readonly string[];
}
