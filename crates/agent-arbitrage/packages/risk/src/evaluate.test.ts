import { describe, it, expect } from 'vitest';
import {
  createToken,
  createPair,
  createSpreadOpportunity,
  type SpreadOpportunity,
} from '@arbitrage-agent/shared';
import { evaluate } from './evaluate.js';
import { createInitialState, engageKillSwitch } from './state.js';
import type { RiskConfig, RiskState } from './types.js';

const CC = createToken('CC', 'Canton Coin', 6, 'canton');
const USDT = createToken('USDT', 'Tether USD', 6, 'cex');
const pair = createPair(CC, USDT, 'bybit');

const config: RiskConfig = {
  maxSpreadBps: 500, // 5%
  maxQuoteAgeMs: 5000,
  dailyLossLimitUsd: 100,
  maxConsecutiveLosses: 3,
};

const NOW = 1_730_000_000_000;

function makeOpp(spreadPct: number, quoteTs: number = NOW): SpreadOpportunity {
  return createSpreadOpportunity(
    pair,
    { venueId: 'bybit', buyPrice: 1.0, sellPrice: 0.99, timestamp: quoteTs },
    { venueId: 'kucoin', buyPrice: 1.0 + 0.01 * spreadPct, sellPrice: 1.0 * (1 + spreadPct / 100), timestamp: quoteTs },
    100,
  );
}

describe('evaluate', () => {
  it('passes a normal opportunity', () => {
    const state = createInitialState(NOW);
    const verdict = evaluate(makeOpp(0.5), state, config, NOW);
    expect(verdict.ok).toBe(true);
  });

  it('rejects when kill switch is engaged', () => {
    const state = engageKillSwitch(createInitialState(NOW), 'manual_panic');
    const verdict = evaluate(makeOpp(0.5), state, config, NOW);
    expect(verdict.ok).toBe(false);
    if (verdict.ok) throw new Error('unreachable');
    expect(verdict.reason).toMatch(/kill_switch_engaged.*manual_panic/);
  });

  it('rejects a bogus large spread (> maxSpreadBps)', () => {
    const state = createInitialState(NOW);
    const verdict = evaluate(makeOpp(10), state, config, NOW); // 10% = 1000 bps > 500
    expect(verdict.ok).toBe(false);
    if (verdict.ok) throw new Error('unreachable');
    expect(verdict.reason).toMatch(/bogus_spread/);
  });

  it('accepts a spread exactly at maxSpreadBps', () => {
    const state = createInitialState(NOW);
    // 5% spread = exactly 500 bps (with float rounding tolerance)
    const verdict = evaluate(makeOpp(5), state, config, NOW);
    expect(verdict.ok).toBe(true);
  });

  it('rejects stale quotes (age > maxQuoteAgeMs)', () => {
    const state = createInitialState(NOW);
    const oldTs = NOW - 10_000; // 10 seconds old, max is 5 seconds
    const verdict = evaluate(makeOpp(0.5, oldTs), state, config, NOW);
    expect(verdict.ok).toBe(false);
    if (verdict.ok) throw new Error('unreachable');
    expect(verdict.reason).toMatch(/stale_quote.*10000 ms/);
  });

  it('accepts quote exactly at maxQuoteAgeMs', () => {
    const state = createInitialState(NOW);
    const ts = NOW - config.maxQuoteAgeMs;
    const verdict = evaluate(makeOpp(0.5, ts), state, config, NOW);
    expect(verdict.ok).toBe(true);
  });

  it('rejects when daily loss limit is reached', () => {
    const state: RiskState = {
      ...createInitialState(NOW),
      realizedPnlTodayUsd: -100, // exactly at -dailyLossLimitUsd
    };
    const verdict = evaluate(makeOpp(0.5), state, config, NOW);
    expect(verdict.ok).toBe(false);
    if (verdict.ok) throw new Error('unreachable');
    expect(verdict.reason).toMatch(/daily_loss_limit/);
  });

  it('rejects when consecutive losses reach limit', () => {
    const state: RiskState = {
      ...createInitialState(NOW),
      consecutiveLosses: 3,
    };
    const verdict = evaluate(makeOpp(0.5), state, config, NOW);
    expect(verdict.ok).toBe(false);
    if (verdict.ok) throw new Error('unreachable');
    expect(verdict.reason).toMatch(/consecutive_losses/);
  });

  it('consecutive-loss check is disabled when maxConsecutiveLosses=0', () => {
    const disabledConfig: RiskConfig = { ...config, maxConsecutiveLosses: 0 };
    const state: RiskState = { ...createInitialState(NOW), consecutiveLosses: 999 };
    const verdict = evaluate(makeOpp(0.5), state, disabledConfig, NOW);
    expect(verdict.ok).toBe(true);
  });
});
