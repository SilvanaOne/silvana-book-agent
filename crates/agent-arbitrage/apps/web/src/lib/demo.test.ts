import { describe, it, expect } from 'vitest';
import { DEMO_TOKENS, DEMO_PAIRS, demoSpreads, demoProfitability, demoSupplementalSpreads, CHART_PAIR_KEYS } from './demo';
import { UI_TOKENS } from './tokens';

describe('demo data', () => {
  it('exposes the requested token set (CC, CBTC, USDC, CETH, USDCx)', () => {
    expect([...DEMO_TOKENS]).toEqual([...UI_TOKENS]);
    expect([...DEMO_TOKENS]).toEqual(['CC', 'CBTC', 'USDC', 'CETH', 'USDCx']);
  });

  it('every demo pair uses tokens from the demo set', () => {
    for (const p of DEMO_PAIRS) {
      expect(DEMO_TOKENS).toContain(p.base);
      expect(DEMO_TOKENS).toContain(p.quote);
    }
  });

  it('demoSpreads has multiple pairs with enough ticks for a chart line', () => {
    const rows = demoSpreads();
    const pairKeys = new Set(rows.map((r) => r.basePairKey));
    expect(pairKeys.size).toBeGreaterThanOrEqual(2);
    for (const key of pairKeys) {
      expect(rows.filter((r) => r.basePairKey === key).length).toBeGreaterThanOrEqual(2);
    }
  });

  it('demoSpreads includes CETH, CBTC, and USDCx in chart pair dropdown source', () => {
    const pairKeys = new Set(demoSpreads().map((r) => r.basePairKey));
    expect(pairKeys.has('CETH/USDC')).toBe(true);
    expect(pairKeys.has('CC/USDCx')).toBe(true);
    expect(pairKeys.has('CC/CBTC')).toBe(true);
    expect(pairKeys.has('CBTC/USDC')).toBe(true);
    expect([...pairKeys].sort()).toEqual([...CHART_PAIR_KEYS].sort());
  });

  it('demo chart curves move smoothly (no sawtooth jumps)', () => {
    for (const pairKey of CHART_PAIR_KEYS) {
      const bps = demoSpreads()
        .filter((r) => r.basePairKey === pairKey)
        .sort((a, b) => new Date(a.ts).getTime() - new Date(b.ts).getTime())
        .map((r) => r.spreadBps);
      for (let i = 1; i < bps.length; i++) {
        expect(Math.abs(bps[i]! - bps[i - 1]!)).toBeLessThanOrEqual(8);
      }
    }
  });

  it('demoSpreads buy/sell prices imply the stored spreadBps', () => {
    for (const r of demoSpreads()) {
      const buy = Number(r.buyPrice);
      const sell = Number(r.sellPrice);
      const observed = ((sell - buy) / buy) * 10_000;
      expect(observed).toBeCloseTo(r.spreadBps, 0);
    }
  });

  it('demoProfitability puts Silvana first', () => {
    const p = demoProfitability();
    expect(p.byVenue[0]!.venueId).toBe('silvana');
  });

  it('demoProfitability totals match the by-size sum (within ~10%)', () => {
    const p = demoProfitability();
    const sum = p.bySize.reduce((s, b) => s + b.count, 0);
    expect(Math.abs(sum - p.totals.count)).toBeLessThanOrEqual(5);
  });

  it('demoSupplementalSpreads includes CETH, CBTC, and USDCx pairs', () => {
    const rows = demoSupplementalSpreads();
    const pairs = new Set(rows.map((r) => r.basePairKey));
    expect([...pairs].some((p) => p.includes('CETH'))).toBe(true);
    expect([...pairs].some((p) => p.includes('CBTC'))).toBe(true);
    expect([...pairs].some((p) => p.includes('USDCx'))).toBe(true);
    for (const r of rows) expect(r.id.startsWith('demo-sup-')).toBe(true);
  });
});
