import type { RiskConfig, RiskState, TradeResult } from './types.js';

const MS_PER_DAY = 86_400_000;

/** UTC midnight boundary preceding the given epoch ms. */
export function utcDayStart(ms: number): number {
  return Math.floor(ms / MS_PER_DAY) * MS_PER_DAY;
}

export function createInitialState(now: number = Date.now()): RiskState {
  return Object.freeze({
    killSwitchEngaged: false,
    realizedPnlTodayUsd: 0,
    pnlPeriodStart: utcDayStart(now),
    consecutiveLosses: 0,
    autoEngagedReason: null,
  });
}

/** Manual kill-switch engage by operator. */
export function engageKillSwitch(state: RiskState, reason: string): RiskState {
  return Object.freeze({
    ...state,
    killSwitchEngaged: true,
    autoEngagedReason: state.autoEngagedReason ?? reason,
  });
}

/** Manual reset (operator unblock after investigation). */
export function disengageKillSwitch(state: RiskState): RiskState {
  return Object.freeze({
    ...state,
    killSwitchEngaged: false,
    autoEngagedReason: null,
  });
}

/**
 * Apply a trade result to the running state.
 *
 * - Rolls the daily counters if UTC day changed.
 * - Updates consecutive-loss streak (resets on a win, increments on a loss,
 *   preserved on break-even).
 * - Auto-engages the kill-switch when a hard limit trips.
 *
 * Idempotent for the inputs — callers must guarantee each `result` is
 * delivered exactly once.
 */
export function recordTradeResult(
  state: RiskState,
  result: TradeResult,
  config: RiskConfig,
  now: number = Date.now(),
): RiskState {
  const currentPeriod = utcDayStart(now);

  let realizedPnlTodayUsd = state.realizedPnlTodayUsd;
  let consecutiveLosses = state.consecutiveLosses;

  if (currentPeriod !== state.pnlPeriodStart) {
    realizedPnlTodayUsd = 0;
    consecutiveLosses = 0;
  }

  realizedPnlTodayUsd += result.realizedPnlUsd;

  if (result.realizedPnlUsd < 0) {
    consecutiveLosses += 1;
  } else if (result.realizedPnlUsd > 0) {
    consecutiveLosses = 0;
  }
  // realizedPnlUsd === 0 — break-even, leave streak unchanged

  let killSwitchEngaged = state.killSwitchEngaged;
  let autoEngagedReason = state.autoEngagedReason;

  if (realizedPnlTodayUsd <= -config.dailyLossLimitUsd) {
    killSwitchEngaged = true;
    autoEngagedReason =
      autoEngagedReason ?? `daily_loss_limit: ${realizedPnlTodayUsd.toFixed(2)} USD`;
  } else if (
    config.maxConsecutiveLosses > 0 &&
    consecutiveLosses >= config.maxConsecutiveLosses
  ) {
    killSwitchEngaged = true;
    autoEngagedReason = autoEngagedReason ?? `consecutive_losses: ${consecutiveLosses}`;
  }

  return Object.freeze({
    killSwitchEngaged,
    realizedPnlTodayUsd,
    pnlPeriodStart: currentPeriod,
    consecutiveLosses,
    autoEngagedReason,
  });
}
