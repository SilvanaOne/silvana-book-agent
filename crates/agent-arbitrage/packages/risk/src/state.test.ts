import { describe, it, expect } from 'vitest';
import {
  createInitialState,
  recordTradeResult,
  engageKillSwitch,
  disengageKillSwitch,
  utcDayStart,
} from './state.js';
import type { RiskConfig } from './types.js';

const config: RiskConfig = {
  maxSpreadBps: 500,
  maxQuoteAgeMs: 5000,
  dailyLossLimitUsd: 100,
  maxConsecutiveLosses: 3,
};

// 2026-05-26 00:00:00 UTC
const DAY1 = Date.UTC(2026, 4, 26, 0, 0, 0);
const DAY1_NOON = Date.UTC(2026, 4, 26, 12, 0, 0);
const DAY2 = Date.UTC(2026, 4, 27, 0, 0, 0);

describe('utcDayStart', () => {
  it('returns midnight UTC for any time within the day', () => {
    expect(utcDayStart(DAY1)).toBe(DAY1);
    expect(utcDayStart(DAY1_NOON)).toBe(DAY1);
    expect(utcDayStart(DAY2 - 1)).toBe(DAY1);
    expect(utcDayStart(DAY2)).toBe(DAY2);
  });
});

describe('createInitialState', () => {
  it('starts neutral', () => {
    const s = createInitialState(DAY1_NOON);
    expect(s.killSwitchEngaged).toBe(false);
    expect(s.realizedPnlTodayUsd).toBe(0);
    expect(s.consecutiveLosses).toBe(0);
    expect(s.autoEngagedReason).toBeNull();
    expect(s.pnlPeriodStart).toBe(DAY1);
  });
});

describe('engageKillSwitch / disengageKillSwitch', () => {
  it('engages with reason', () => {
    const s = engageKillSwitch(createInitialState(DAY1_NOON), 'manual_panic');
    expect(s.killSwitchEngaged).toBe(true);
    expect(s.autoEngagedReason).toBe('manual_panic');
  });

  it('disengages clears reason', () => {
    const s = disengageKillSwitch(engageKillSwitch(createInitialState(DAY1_NOON), 'r'));
    expect(s.killSwitchEngaged).toBe(false);
    expect(s.autoEngagedReason).toBeNull();
  });
});

describe('recordTradeResult', () => {
  it('accumulates PnL', () => {
    let s = createInitialState(DAY1_NOON);
    s = recordTradeResult(s, { realizedPnlUsd: 5 }, config, DAY1_NOON);
    s = recordTradeResult(s, { realizedPnlUsd: -3 }, config, DAY1_NOON);
    expect(s.realizedPnlTodayUsd).toBeCloseTo(2, 6);
  });

  it('rolls daily counters at UTC day boundary', () => {
    let s = createInitialState(DAY1_NOON);
    s = recordTradeResult(s, { realizedPnlUsd: -50 }, config, DAY1_NOON);
    expect(s.realizedPnlTodayUsd).toBe(-50);
    expect(s.consecutiveLosses).toBe(1);

    // Day rolls over
    s = recordTradeResult(s, { realizedPnlUsd: 10 }, config, DAY2);
    expect(s.realizedPnlTodayUsd).toBe(10);
    expect(s.consecutiveLosses).toBe(0);
    expect(s.pnlPeriodStart).toBe(DAY2);
  });

  it('increments consecutive losses on loss, resets on win', () => {
    let s = createInitialState(DAY1_NOON);
    s = recordTradeResult(s, { realizedPnlUsd: -1 }, config, DAY1_NOON);
    expect(s.consecutiveLosses).toBe(1);
    s = recordTradeResult(s, { realizedPnlUsd: -1 }, config, DAY1_NOON);
    expect(s.consecutiveLosses).toBe(2);
    s = recordTradeResult(s, { realizedPnlUsd: 1 }, config, DAY1_NOON);
    expect(s.consecutiveLosses).toBe(0);
  });

  it('preserves consecutive losses on break-even (0 PnL)', () => {
    let s = createInitialState(DAY1_NOON);
    s = recordTradeResult(s, { realizedPnlUsd: -1 }, config, DAY1_NOON);
    s = recordTradeResult(s, { realizedPnlUsd: 0 }, config, DAY1_NOON);
    expect(s.consecutiveLosses).toBe(1);
  });

  it('auto-engages kill switch on daily loss limit', () => {
    let s = createInitialState(DAY1_NOON);
    s = recordTradeResult(s, { realizedPnlUsd: -100 }, config, DAY1_NOON);
    expect(s.killSwitchEngaged).toBe(true);
    expect(s.autoEngagedReason).toMatch(/daily_loss_limit/);
  });

  it('auto-engages kill switch on max consecutive losses', () => {
    let s = createInitialState(DAY1_NOON);
    s = recordTradeResult(s, { realizedPnlUsd: -1 }, config, DAY1_NOON);
    s = recordTradeResult(s, { realizedPnlUsd: -1 }, config, DAY1_NOON);
    s = recordTradeResult(s, { realizedPnlUsd: -1 }, config, DAY1_NOON);
    expect(s.killSwitchEngaged).toBe(true);
    expect(s.autoEngagedReason).toMatch(/consecutive_losses/);
  });

  it('does not double-update autoEngagedReason on subsequent trips', () => {
    let s = createInitialState(DAY1_NOON);
    s = recordTradeResult(s, { realizedPnlUsd: -100 }, config, DAY1_NOON);
    const firstReason = s.autoEngagedReason;
    s = recordTradeResult(s, { realizedPnlUsd: -10 }, config, DAY1_NOON);
    expect(s.autoEngagedReason).toBe(firstReason);
  });
});
