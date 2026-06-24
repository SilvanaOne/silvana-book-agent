/**
 * Scan cycle — orchestrates parallel fetch across venues + analysis.
 *
 * Adapted from dex-arbitrager `src/usecases/SpreadScanCycle.ts` + `FetchPricesUseCase.ts`.
 */

import type { Pair, DexQuote, IVenueProvider } from '@arbitrage-agent/shared';
import type { TradeConfig, ScanCycleResult } from './types.js';
import { analyzeAllPairs } from './spread-detector.js';
import { analyzeCrossCluster, type StableRouterConfig } from './cross-cluster.js';

interface FetchResult {
  readonly results: ReadonlyMap<Pair, readonly DexQuote[]>;
  readonly errors: readonly string[];
}

/**
 * Fetch quotes for all (pair, provider) combinations in parallel.
 * Returns a map keyed by Pair → array of DexQuotes (one per provider that supports it).
 */
export async function fetchAllPrices(
  pairs: readonly Pair[],
  providers: readonly IVenueProvider[],
): Promise<FetchResult> {
  const tasks: Array<Promise<{ pair: Pair; quote: DexQuote | null; error?: string }>> = [];

  for (const pair of pairs) {
    for (const provider of providers) {
      if (!provider.supportsPair(pair)) continue;
      tasks.push(
        provider
          .getQuote(pair)
          .then((quote) => ({ pair, quote }))
          .catch((err: unknown) => ({
            pair,
            quote: null,
            error: `${provider.name}: ${err instanceof Error ? err.message : String(err)}`,
          })),
      );
    }
  }

  const settled = await Promise.all(tasks);
  const results = new Map<Pair, DexQuote[]>();
  const errors: string[] = [];

  for (const r of settled) {
    if (r.error) errors.push(r.error);
    if (r.quote) {
      const bucket = results.get(r.pair) ?? [];
      bucket.push(r.quote);
      results.set(r.pair, bucket);
    }
  }

  return { results, errors };
}

/**
 * End-to-end scan cycle: fetch quotes, analyse spreads, return opportunities.
 *
 * When `crossCluster` is enabled, also runs the CC-triangulation analyzer so
 * spreads between different stablecoin clusters (e.g. CC/USDCx vs CC/USDC) are
 * detected net of conversion cost, and merged into `opportunities`.
 */
export async function runSpreadScanCycle(args: {
  readonly pairs: readonly Pair[];
  readonly providers: readonly IVenueProvider[];
  readonly config: TradeConfig;
  readonly crossCluster?: boolean;
  readonly stableRouter?: StableRouterConfig;
}): Promise<ScanCycleResult> {
  const { results: quoteMap, errors: fetchErrors } = await fetchAllPrices(args.pairs, args.providers);
  const sameStable = analyzeAllPairs(quoteMap, args.config);
  const crossStable = args.crossCluster
    ? analyzeCrossCluster(quoteMap, args.config, args.stableRouter)
    : [];
  const opportunities = Object.freeze(
    [...sameStable, ...crossStable].sort((a, b) => b.spreadPct - a.spreadPct),
  );
  return { quoteMap, opportunities, fetchErrors };
}
