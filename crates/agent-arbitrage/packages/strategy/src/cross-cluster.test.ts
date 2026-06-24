import { describe, it, expect } from 'vitest';
import { createToken, createPair, type Pair, type DexQuote } from '@arbitrage-agent/shared';
import { analyzeCrossCluster, DEFAULT_STABLE_ROUTER } from './cross-cluster.js';
import type { TradeConfig } from './types.js';

const cc = createToken('CC', 'Canton Coin', 6, 'canton');
const usdcx = createToken('USDCx', 'USDC (Canton)', 6, 'canton');
const usdc = createToken('USDC', 'USD Coin', 6, 'canton');

const config: TradeConfig = {
  targetSpreadPercent: 0.25,
  maxSpreadPercent: 10,
  tradeSizeUsd: 100,
};

function quote(venueId: DexQuote['venueId'], buyPrice: number, sellPrice: number): DexQuote {
  return Object.freeze({ venueId, buyPrice, sellPrice, timestamp: Date.now() });
}

describe('analyzeCrossCluster', () => {
  it('detects a CC spread across USDCx and USDC clusters net of conversion', () => {
    // Cantex (USDCx) CC ask 0.0198594; Silvana (USDC) CC bid 0.0205794.
    const map = new Map<Pair, readonly DexQuote[]>([
      [createPair(cc, usdcx, 'cantex'), [quote('cantex', 0.0198594, 0.019741)]],
      [createPair(cc, usdc, 'silvana'), [quote('silvana', 0.0206206, 0.0205794)]],
    ]);
    const opps = analyzeCrossCluster(map, config);
    expect(opps.length).toBe(1);
    const o = opps[0]!;
    // gross = (0.0205794-0.0198594)/0.0198594 = 3.63%; minus 30bps conv = ~3.33%
    expect(o.spreadPct).toBeGreaterThan(3.2);
    expect(o.spreadPct).toBeLessThan(3.4);
    expect(o.buyFrom.venueId).toBe('cantex');
    expect(o.sellTo.venueId).toBe('silvana');
    // synthetic cross-cluster quote token marks the pair
    expect(o.pair.quote.symbol).toBe('USDCx~USDC');
  });

  it('does NOT pair same-stable quotes (left to the normal detector)', () => {
    const map = new Map<Pair, readonly DexQuote[]>([
      [createPair(cc, usdcx, 'oneswap'), [quote('oneswap', 0.02006, 0.01994)]],
      [createPair(cc, usdcx, 'temple'), [quote('temple', 0.0205205, 0.0204795)]],
    ]);
    expect(analyzeCrossCluster(map, config).length).toBe(0);
  });

  it('rejects when conversion cost eats the gross spread', () => {
    // gross ~0.29% but USDCx->USDC conv is 30bps → net negative.
    const map = new Map<Pair, readonly DexQuote[]>([
      [createPair(cc, usdcx, 'temple'), [quote('temple', 0.0205205, 0.0204795)]],
      [createPair(cc, usdc, 'silvana'), [quote('silvana', 0.0206206, 0.0205794)]],
    ]);
    expect(analyzeCrossCluster(map, config).length).toBe(0);
  });

  it('ignores non-stable quote pairs', () => {
    const usdt = createToken('USDT', 'Tether', 6, 'cex');
    const eth = createToken('ETH', 'Ether', 18, 'cex');
    const map = new Map<Pair, readonly DexQuote[]>([
      [createPair(cc, usdt, 'bybit'), [quote('bybit', 0.02, 0.0199)]],
      [createPair(cc, eth, 'bybit'), [quote('bybit', 0.02, 0.0199)]],
    ]);
    // CC/USDT alone has no cross-stable counterpart here → no cross-cluster opp.
    expect(analyzeCrossCluster(map, config).length).toBe(0);
  });

  it('honours custom conversion matrix', () => {
    const map = new Map<Pair, readonly DexQuote[]>([
      [createPair(cc, usdcx, 'cantex'), [quote('cantex', 0.0198594, 0.019741)]],
      [createPair(cc, usdc, 'silvana'), [quote('silvana', 0.0206206, 0.0205794)]],
    ]);
    const cheap = analyzeCrossCluster(map, config, {
      ...DEFAULT_STABLE_ROUTER,
      conversionCostBps: { 'USDC->USDCx': 0, 'USDCx->USDC': 0 },
    });
    // No conversion cost → net == gross (~3.63%)
    expect(cheap[0]!.spreadPct).toBeGreaterThan(3.6);
  });
});
