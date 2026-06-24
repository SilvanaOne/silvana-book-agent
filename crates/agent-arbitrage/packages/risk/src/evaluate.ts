import type { SpreadOpportunity } from '@arbitrage-agent/shared';
import type { RiskConfig, RiskState, RiskVerdict } from './types.js';

/**
 * Pre-trade safety check. Pure function — does not mutate state.
 *
 * Order of checks is intentional: kill-switch first (cheapest, blocks
 * everything), then per-opportunity validity (bogus spread, stale quote),
 * then cumulative state (daily loss, consecutive losses).
 */
export function evaluate(
  opp: SpreadOpportunity,
  state: RiskState,
  config: RiskConfig,
  now: number = Date.now(),
): RiskVerdict {
  if (state.killSwitchEngaged) {
    return {
      ok: false,
      reason: state.autoEngagedReason
        ? `kill_switch_engaged (${state.autoEngagedReason})`
        : 'kill_switch_engaged',
    };
  }

  const spreadBps = Math.round(opp.spreadPct * 100);
  if (spreadBps > config.maxSpreadBps) {
    return {
      ok: false,
      reason: `bogus_spread: ${spreadBps} bps > max ${config.maxSpreadBps} bps`,
    };
  }

  const oldestQuoteTs = Math.min(opp.buyFrom.timestamp, opp.sellTo.timestamp);
  const ageMs = now - oldestQuoteTs;
  if (ageMs > config.maxQuoteAgeMs) {
    return {
      ok: false,
      reason: `stale_quote: age ${ageMs} ms > max ${config.maxQuoteAgeMs} ms`,
    };
  }

  if (state.realizedPnlTodayUsd <= -config.dailyLossLimitUsd) {
    return {
      ok: false,
      reason:
        `daily_loss_limit: PnL ${state.realizedPnlTodayUsd.toFixed(2)} USD ` +
        `<= -${config.dailyLossLimitUsd}`,
    };
  }

  if (
    config.maxConsecutiveLosses > 0 &&
    state.consecutiveLosses >= config.maxConsecutiveLosses
  ) {
    return {
      ok: false,
      reason: `consecutive_losses: ${state.consecutiveLosses} >= max ${config.maxConsecutiveLosses}`,
    };
  }

  return { ok: true };
}
