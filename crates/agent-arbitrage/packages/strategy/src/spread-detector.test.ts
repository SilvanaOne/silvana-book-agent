import { describe, it, expect } from 'vitest';
import {
  createToken,
  createPair,
  type DexQuote,
  type Pair,
} from '@arbitrage-agent/shared';
import { findSpreads, findBestSpread, analyzeAllPairs } from './spread-detector.js';
import type { TradeConfig } from './types.js';

const CC = createToken('CC', 'Canton Coin', 6, 'canton');
const USDT = createToken('USDT', 'Tether USD', 6, 'cex');

const config: TradeConfig = {
  targetSpreadPercent: 0.25, // 25 bps
  maxSpreadPercent: 10,
  tradeSizeUsd: 100,
};

const quote = (venueId: 'bybit' | 'kucoin', buy: number, sell: number): DexQuote => ({
  venueId,
  buyPrice: buy,
  sellPrice: sell,
  timestamp: Date.now(),
});

const pair: Pair = createPair(CC, USDT, 'bybit');

describe('findSpreads', () => {
  it('returns empty when fewer than 2 quotes', () => {
    expect(findSpreads(pair, [], config)).toEqual([]);
    expect(findSpreads(pair, [quote('bybit', 1, 1)], config)).toEqual([]);
  });

  it('detects spread above threshold', () => {
    const quotes = [quote('bybit', 1.0, 1.0), quote('kucoin', 1.01, 1.02)];
    const result = findSpreads(pair, quotes, config);
    expect(result).toHaveLength(1);
    expect(result[0]!.spreadPct).toBeCloseTo(2.0, 4);
    expect(result[0]!.buyFrom.venueId).toBe('bybit');
    expect(result[0]!.sellTo.venueId).toBe('kucoin');
  });

  it('rejects spread below threshold', () => {
    const quotes = [quote('bybit', 1.0, 1.0), quote('kucoin', 1.0001, 1.001)];
    expect(findSpreads(pair, quotes, config)).toEqual([]);
  });

  it('rejects spread above maxSpreadPercent (likely bogus quote)', () => {
    const quotes = [quote('bybit', 1.0, 1.0), quote('kucoin', 2.0, 5.0)];
    expect(findSpreads(pair, quotes, config)).toEqual([]);
  });

  it('rejects negative or zero prices', () => {
    const quotes = [quote('bybit', 0, 0), quote('kucoin', 1.0, 1.5)];
    expect(findSpreads(pair, quotes, config)).toEqual([]);
  });

  it('does not detect spread when buy-ask of each venue exceeds sell-bid of the other', () => {
    // Direction A: buy bybit ask 1.01, sell kucoin bid 1.00 → 1.01 > 1.00, no profit
    // Direction B: buy kucoin ask 1.02, sell bybit bid 0.99 → 1.02 > 0.99, no profit
    const quotes = [quote('bybit', 1.01, 0.99), quote('kucoin', 1.02, 1.0)];
    expect(findSpreads(pair, quotes, config)).toEqual([]);
  });

  it('returns results sorted descending by spreadPct', () => {
    const quotes = [
      quote('bybit', 1.0, 1.0),
      quote('kucoin', 1.01, 1.05), // big spread
      quote('kucoin', 1.001, 1.005), // small spread
    ];
    const result = findSpreads(pair, quotes, config);
    expect(result.length).toBeGreaterThanOrEqual(2);
    for (let i = 1; i < result.length; i++) {
      expect(result[i - 1]!.spreadPct).toBeGreaterThanOrEqual(result[i]!.spreadPct);
    }
  });

  it('returns a frozen array', () => {
    const quotes = [quote('bybit', 1.0, 1.0), quote('kucoin', 1.01, 1.02)];
    const result = findSpreads(pair, quotes, config);
    expect(Object.isFrozen(result)).toBe(true);
  });
});

describe('findBestSpread', () => {
  it('returns null when no opportunities', () => {
    expect(findBestSpread(pair, [], config)).toBeNull();
  });

  it('returns the single highest-spread opportunity', () => {
    const quotes = [
      quote('bybit', 1.0, 1.0),
      quote('kucoin', 1.01, 1.05), // spread 5%
      quote('kucoin', 1.0, 1.01), // spread 1%
    ];
    const best = findBestSpread(pair, quotes, config);
    expect(best).not.toBeNull();
    expect(best!.spreadPct).toBeCloseTo(5.0, 4);
  });
});

describe('analyzeAllPairs', () => {
  it('groups quotes by canonical token-pair key across venues', () => {
    const pairOnBybit = createPair(CC, USDT, 'bybit');
    const pairOnKuCoin = createPair(CC, USDT, 'kucoin');
    const quotes = new Map<Pair, readonly DexQuote[]>([
      [pairOnBybit, [quote('bybit', 1.0, 1.0)]],
      [pairOnKuCoin, [quote('kucoin', 1.01, 1.05)]],
    ]);
    const result = analyzeAllPairs(quotes, config);
    expect(result.length).toBeGreaterThan(0);
    // Both venues compete in the same pair key 'CC/USDT'
    expect(result[0]!.spreadPct).toBeCloseTo(5.0, 4);
  });
});
