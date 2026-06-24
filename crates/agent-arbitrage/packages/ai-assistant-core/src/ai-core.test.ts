import { describe, expect, it } from 'vitest';
import { ApprovalManager } from './approval.js';
import { CostGuard } from './cost.js';
import { configSuggest, resolveSymbol, tradePostmortem } from './features.js';
import { makeProvider } from './providers.js';
import type { TradeRecord } from './types.js';

const base: TradeRecord = { ticker: 'CC/USDT', buyVenueId: 'bybit', sellVenueId: 'kucoin', status: 'SUCCESS' };

describe('tradePostmortem', () => {
  it('explains a partial fill and suggests a hedge', () => {
    const r = tradePostmortem({ ...base, status: 'PARTIAL', partialExecution: true, failReason: 'sell rejected' });
    expect(r.whatHappened).toMatch(/Buy leg filled/);
    expect(r.suggestedActions.some((a) => /hedge/i.test(a.description))).toBe(true);
  });
  it('marks success with no actions', () => {
    expect(tradePostmortem(base).suggestedActions).toHaveLength(0);
  });
  it('handles failed (no capital moved)', () => {
    const r = tradePostmortem({ ...base, status: 'FAILED', failReason: 'buy threw' });
    expect(r.whyItFailed).toMatch(/buy/i);
  });
});

describe('configSuggest', () => {
  it('parses min spread bps + disable venue into a proposed diff + approval', () => {
    const appr = new ApprovalManager();
    const r = configSuggest('set min spread to 30 bps and disable hyperliquid', {}, appr);
    expect(r.confirmationRequired).toBe(true);
    const paths = r.proposedDiff.map((d) => d.path);
    expect(paths).toContain('tradeConfig.targetSpreadPercent');
    expect(paths).toContain('venues.hyperliquid.enabled');
    const spread = r.proposedDiff.find((d) => d.path === 'tradeConfig.targetSpreadPercent');
    expect(spread!.new).toBe(0.3); // 30 bps
    // approval is pending and confirmable
    expect(appr.confirm(r.approvalId)).toEqual(r.proposedDiff);
  });
  it('explains when nothing parseable', () => {
    const r = configSuggest('make it better please', {}, new ApprovalManager());
    expect(r.proposedDiff).toHaveLength(0);
  });
});

describe('resolveSymbol', () => {
  it('single confident mapping for CC', () => {
    const r = resolveSymbol('CC');
    expect(r.candidates).toHaveLength(1);
    expect(r.requiresOperatorChoice).toBe(false);
  });
  it('ambiguous for USDC', () => {
    expect(resolveSymbol('USDC').requiresOperatorChoice).toBe(true);
  });
  it('unknown → no candidates', () => {
    expect(resolveSymbol('zzz').candidates).toHaveLength(0);
  });
});

describe('ApprovalManager', () => {
  it('confirm once, reject/confirm afterward is a no-op', () => {
    const m = new ApprovalManager();
    const a = m.create('config-diff', { x: 1 });
    expect(m.confirm(a.id)).toEqual({ x: 1 });
    expect(m.confirm(a.id)).toBeNull();
    expect(m.reject(a.id)).toBe(false);
  });
  it('expires after TTL', () => {
    let t = 1000;
    const m = new ApprovalManager(100, () => t);
    const a = m.create('k', {});
    t = 2000;
    expect(m.get(a.id)!.status).toBe('expired');
    expect(m.confirm(a.id)).toBeNull();
  });
});

describe('CostGuard', () => {
  it('rate-limits per minute', () => {
    let t = 0;
    const g = new CostGuard(500, 3, () => t);
    expect(g.check('op').ok).toBe(true);
    expect(g.check('op').ok).toBe(true);
    expect(g.check('op').ok).toBe(true);
    expect(g.check('op').ok).toBe(false); // 4th within the minute
  });
});

describe('providers', () => {
  it('heuristic → null (deterministic path); real providers are stubbed', async () => {
    expect(makeProvider('heuristic')).toBeNull();
    const p = makeProvider('anthropic')!;
    await expect(p.complete({ system: 's', user: 'u' })).rejects.toThrow(/not wired/);
  });
});
