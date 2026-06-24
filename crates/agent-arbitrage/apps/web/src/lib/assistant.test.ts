import { describe, it, expect } from 'vitest';
import { advise, PRESET_QUESTIONS, type AdviceContext } from './assistant';
import type { SpreadDto, RuntimeConfig, Stats } from './api';

function spread(over: Partial<SpreadDto>): SpreadDto {
  return {
    id: '1',
    ts: new Date().toISOString(),
    basePairKey: 'CC/USDCx',
    buyVenueId: 'oneswap',
    sellVenueId: 'temple',
    buyPrice: '0.02',
    sellPrice: '0.0205',
    spreadBps: 200,
    estProfitUsd: '2',
    acted: false,
    ...over,
  };
}

const config: RuntimeConfig = {
  tradeConfig: { targetSpreadPercent: 0.25, maxSpreadPercent: 10, tradeSizeUsd: 100 },
  riskConfig: { maxSpreadBps: 500, maxQuoteAgeMs: 5000, dailyLossLimitUsd: 100, maxConsecutiveLosses: 3 },
  runtime: { strategyMode: 'paper', silvanaHostMode: 'standalone-dev', scanIntervalMs: 5000, scannerPersist: true, killSwitch: false },
};

const stats: Stats = {
  windowMs: 3_600_000,
  spreads: { total: 10, lastHour: 8, avgBps: 180, maxBps: 333, byRoute: [] },
  pnl: { actedCount: 0, estimatedUsd: '0', realizedUsd: '0' },
};

const ctx: AdviceContext = {
  spreads: [spread({ spreadBps: 120 }), spread({ id: '2', spreadBps: 333, basePairKey: 'CC/USDCx~USDC', buyVenueId: 'cantex', sellVenueId: 'silvana' })],
  config,
  stats,
};

describe('advise', () => {
  it('reports the widest spread and flags cross-cluster', () => {
    const a = advise('what is the best spread right now?', ctx);
    expect(a.text).toContain('333 bps');
    expect(a.text).toContain('cantex');
    expect(a.text.toLowerCase()).toContain('cross-cluster');
  });

  it('summarises opportunities with the cross-cluster count', () => {
    const a = advise('summarise current opportunities', ctx);
    expect(a.text).toContain('2 spreads');
    expect(a.text).toContain('1 cross-cluster');
  });

  it('suggests a target spread and returns an inert suggestion', () => {
    const a = advise('suggest a target spread', ctx);
    expect(a.suggestion).toBeDefined();
    // avg 180bps * 0.6 = 108bps → 1.08%
    expect(a.suggestion!.detail).toContain('1.08%');
    expect(a.text).toContain('average');
  });

  it('explains risk rejection deterministically (no execution claim)', () => {
    const a = advise('why might an opportunity be rejected?', ctx);
    expect(a.text).toContain('500 bps');
    expect(a.text).toContain('5000 ms');
    expect(a.suggestion).toBeUndefined();
  });

  it('handles the empty-spreads case gracefully', () => {
    const a = advise('best spread', { spreads: [], config, stats: null });
    expect(a.text.toLowerCase()).toContain('no live spreads');
  });

  it('falls back to a capability summary for unknown questions', () => {
    const a = advise('hello there', ctx);
    expect(a.text.toLowerCase()).toContain('advise');
  });

  it('ships a non-empty preset prompt set', () => {
    expect(PRESET_QUESTIONS.length).toBeGreaterThanOrEqual(4);
  });
});
