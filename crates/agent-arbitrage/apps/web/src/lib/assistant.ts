/**
 * Advisory assistant — DETERMINISTIC local heuristics (no LLM call yet).
 *
 * Per the AI-module decisions: AI lives in the advisory layer only — it
 * SUGGESTS, the operator CONFIRMS. The hot trading path stays deterministic.
 * This module is a pure, testable preview of that advisory surface; the
 * Anthropic provider + RU locale wire in with the post-MVP AI module
 * (md_docs/13-ai-assistant-expansion.md). Nothing here places trades or
 * mutates config — suggestions are inert until the operator acts.
 */

import type { SpreadDto, RuntimeConfig, Stats } from './api';

export interface AdviceContext {
  readonly spreads: readonly SpreadDto[];
  readonly config: RuntimeConfig | null;
  readonly stats: Stats | null;
}

export interface Suggestion {
  readonly label: string;
  /** Human-readable proposed change; operator applies it manually on the dashboard. */
  readonly detail: string;
}

export interface Advice {
  readonly text: string;
  readonly suggestion?: Suggestion;
}

export const PRESET_QUESTIONS = [
  'Summarise current opportunities',
  'What is the best spread right now?',
  'Suggest a target spread',
  'Why might an opportunity be rejected?',
  'Is any venue unhealthy?',
] as const;

function topSpread(spreads: readonly SpreadDto[]): SpreadDto | null {
  return spreads.reduce<SpreadDto | null>(
    (best, s) => (best === null || s.spreadBps > best.spreadBps ? s : best),
    null,
  );
}

/** Route a question to a deterministic advisory answer over the current context. */
export function advise(question: string, ctx: AdviceContext): Advice {
  const q = question.toLowerCase();
  const { spreads, config, stats } = ctx;

  if (q.includes('best') || q.includes('top')) {
    const t = topSpread(spreads);
    if (!t) return { text: 'No live spreads are visible yet. Make sure the scanner is running with SCANNER_PERSIST=1.' };
    const cross = t.basePairKey.includes('~');
    return {
      text:
        `The widest current spread is ${t.spreadBps} bps on ${t.basePairKey} ` +
        `(buy ${t.buyVenueId} → sell ${t.sellVenueId}), est. $${Number(t.estProfitUsd).toFixed(2)} on the configured trade size.` +
        (cross ? ' This is a cross-cluster route — its spread is already net of the stablecoin conversion cost.' : ''),
    };
  }

  if (q.includes('suggest') || q.includes('target')) {
    const avg = stats?.spreads.avgBps ?? 0;
    const current = config?.tradeConfig.targetSpreadPercent ?? 0;
    // Heuristic: aim a touch under the recent average so good fills aren't missed,
    // with a sane floor. Purely advisory.
    const proposedBps = Math.max(25, Math.round(avg * 0.6));
    const proposedPct = proposedBps / 100;
    return {
      text:
        `Recent average spread is ~${avg} bps; your target is ${current}% (${Math.round(current * 100)} bps). ` +
        `A target around ${proposedPct}% captures the bulk of moves without over-filtering.`,
      suggestion: {
        label: 'Proposed target spread',
        detail: `Set Trade config → target spread to ${proposedPct}% (≈${proposedBps} bps). Apply it yourself on the Dashboard.`,
      },
    };
  }

  if (q.includes('reject') || q.includes('risk')) {
    const maxBps = config?.riskConfig.maxSpreadBps ?? 0;
    const ageMs = config?.riskConfig.maxQuoteAgeMs ?? 0;
    return {
      text:
        `Opportunities are rejected by the deterministic risk checks, not by me. Common reasons: ` +
        `spread above the max (${maxBps} bps, treated as a bogus quote), a stale quote (older than ${ageMs} ms), ` +
        `the daily loss limit, or consecutive-loss circuit breaker. The kill switch also pauses scanning entirely.`,
    };
  }

  if (q.includes('venue') || q.includes('health') || q.includes('unhealthy') || q.includes('connect')) {
    return {
      text: 'Check the Venue connections panel on the Dashboard — green = healthy, amber = stale, red = down. I read the same /api/venues/health feed it does.',
    };
  }

  if (q.includes('summar') || q.includes('opportunit')) {
    if (spreads.length === 0) return { text: 'No opportunities are visible right now.' };
    const cross = spreads.filter((s) => s.basePairKey.includes('~')).length;
    const t = topSpread(spreads);
    return {
      text:
        `${spreads.length} spreads in view (${cross} cross-cluster). ` +
        `Widest: ${t?.spreadBps ?? 0} bps on ${t?.basePairKey ?? '—'}. ` +
        (stats ? `Over the last hour: ${stats.spreads.lastHour} spreads, avg ${stats.spreads.avgBps} bps, max ${stats.spreads.maxBps} bps.` : ''),
    };
  }

  return {
    text:
      "I can summarise opportunities, point out the widest spread, suggest a target spread, or explain risk rejections. " +
      'I only advise — every change is applied by you on the Dashboard.',
  };
}
