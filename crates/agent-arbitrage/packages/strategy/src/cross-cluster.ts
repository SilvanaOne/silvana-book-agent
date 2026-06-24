/**
 * Cross-cluster spread detection via CC-triangulation (Sprint 7).
 *
 * The Canton venues split into stablecoin clusters that do NOT share a quote
 * asset: the USDCx cluster (OneSwap, Temple, Cantex) and the USDC cluster
 * (Silvana Book). The normal detector groups by exact `base/quote` key, so it
 * never compares CC/USDCx against CC/USDC.
 *
 * But CC is the common base: a CC price difference across clusters is real
 * arbitrage once you account for the cost of converting the proceeds stable
 * back to the funding stable (peg basis + bridge fee). This module models
 * that conversion-cost matrix and reports the NET cross-cluster spread.
 *
 * This is DETECTION (signalling) only — open core. The actual two-leg execution
 * plus the stable-router that picks the cheapest conversion path (direct pool /
 * CC-triangulation / Circle Gateway) live in the proprietary executor +
 * rebalancer. See md_docs/12 §Sprint 7.
 */

import {
  type Pair,
  type DexQuote,
  type SpreadOpportunity,
  createToken,
  createPair,
} from '@arbitrage-agent/shared';
import type { TradeConfig } from './types.js';

export interface StableRouterConfig {
  /** Quote symbols treated as USD-class stables. */
  readonly stables: readonly string[];
  /** Conversion cost in bps, keyed `${from}->${to}`. Symmetric entries expected. */
  readonly conversionCostBps: Readonly<Record<string, number>>;
  /** Fallback cost (bps) for stable pairs not in the matrix. */
  readonly defaultConversionBps: number;
}

/**
 * Default conversion costs (bps). Illustrative — the proprietary rebalancer
 * owns the real, route-aware numbers. USDCx is the Canton-wrapped USDC and
 * carries a bridge/peg basis vs Splice USDC; USDT is a touch further out.
 */
export const DEFAULT_STABLE_ROUTER: StableRouterConfig = {
  stables: ['USDC', 'USDCx', 'USDT'],
  conversionCostBps: {
    'USDCx->USDC': 30,
    'USDC->USDCx': 30,
    'USDCx->USDT': 25,
    'USDT->USDCx': 25,
    'USDC->USDT': 10,
    'USDT->USDC': 10,
  },
  defaultConversionBps: 50,
};

function conversionBps(cfg: StableRouterConfig, from: string, to: string): number {
  if (from === to) return 0;
  return cfg.conversionCostBps[`${from}->${to}`] ?? cfg.defaultConversionBps;
}

interface Entry {
  readonly baseSymbol: string;
  readonly baseDecimals: number;
  readonly stable: string;
  readonly quote: DexQuote;
}

/**
 * Find cross-cluster CC-triangulation opportunities. For each base asset, pairs
 * quotes that are priced in DIFFERENT stables (same-stable cross-venue spreads
 * are already covered by the normal detector) and reports the spread net of the
 * stable→stable conversion cost.
 *
 * The returned opportunities carry a synthetic quote token `${buyStable}~${sellStable}`
 * so persistence/UI can tell cross-cluster rows apart (basePairKey e.g.
 * `CC/USDCx~USDC`). spreadPct is already net of conversion.
 */
export function analyzeCrossCluster(
  pairQuotes: ReadonlyMap<Pair, readonly DexQuote[]>,
  config: TradeConfig,
  router: StableRouterConfig = DEFAULT_STABLE_ROUTER,
): readonly SpreadOpportunity[] {
  const byBase = new Map<string, Entry[]>();
  for (const [pair, quotes] of pairQuotes) {
    if (!router.stables.includes(pair.quote.symbol)) continue;
    const arr = byBase.get(pair.base.symbol) ?? [];
    for (const quote of quotes) {
      arr.push({
        baseSymbol: pair.base.symbol,
        baseDecimals: pair.base.decimals,
        stable: pair.quote.symbol,
        quote,
      });
    }
    byBase.set(pair.base.symbol, arr);
  }

  const out: SpreadOpportunity[] = [];
  for (const entries of byBase.values()) {
    for (let i = 0; i < entries.length; i++) {
      for (let j = 0; j < entries.length; j++) {
        if (i === j) continue;
        const buy = entries[i]!;
        const sell = entries[j]!;
        if (buy.quote.venueId === sell.quote.venueId) continue;
        if (buy.stable === sell.stable) continue; // cross-cluster only

        const askA = buy.quote.buyPrice;
        const bidB = sell.quote.sellPrice;
        if (askA <= 0 || bidB <= 0 || askA >= bidB) continue;

        // Convert proceeds (stableB) back to the funding stable (stableA).
        const convBps = conversionBps(router, sell.stable, buy.stable);
        const grossPct = ((bidB - askA) / askA) * 100;
        const netPct = grossPct - convBps / 100;

        if (netPct < config.targetSpreadPercent) continue;
        if (config.maxSpreadPercent > 0 && netPct > config.maxSpreadPercent) continue;

        const syntheticQuote = createToken(
          `${buy.stable}~${sell.stable}`,
          'cross-cluster',
          buy.baseDecimals,
          'canton',
        );
        const base = createToken(buy.baseSymbol, buy.baseSymbol, buy.baseDecimals, 'canton');
        const pair = createPair(base, syntheticQuote, buy.quote.venueId);

        out.push(
          Object.freeze({
            pair,
            buyFrom: buy.quote,
            sellTo: sell.quote,
            spreadPct: netPct,
            estimatedProfitUsd: (netPct / 100) * config.tradeSizeUsd,
            detectedAt: Date.now(),
          }),
        );
      }
    }
  }

  return Object.freeze(out.sort((a, b) => b.spreadPct - a.spreadPct));
}
