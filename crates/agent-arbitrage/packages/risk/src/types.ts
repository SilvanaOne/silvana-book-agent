/**
 * Risk module — pre-trade safety checks and cumulative state.
 *
 * Lives in open-core. Decisions ("can we forward this opportunity to the
 * executor?") are made here. The actual trade execution lives in the
 * proprietary executor (separate repo).
 *
 * Per md_docs/03-arbitrage-model.md §"Risk limits".
 */

export interface RiskConfig {
  /** Cap on accepted spread (bps). Above this is treated as a bogus quote. */
  readonly maxSpreadBps: number;
  /** Maximum quote age (ms). Older quotes are rejected as stale. */
  readonly maxQuoteAgeMs: number;
  /** Daily realized-loss threshold (positive USD value). When abs(daily PnL) ≥ this, kill-switch trips. */
  readonly dailyLossLimitUsd: number;
  /** Kill-switch trips after this many consecutive losing trades. 0 = disabled. */
  readonly maxConsecutiveLosses: number;
}

export interface RiskState {
  /** Either manually engaged by operator or auto-tripped by hard limit. */
  readonly killSwitchEngaged: boolean;
  /** Cumulative realized PnL since pnlPeriodStart (USD). Negative = loss. */
  readonly realizedPnlTodayUsd: number;
  /** UTC day boundary in epoch ms. */
  readonly pnlPeriodStart: number;
  /** Current streak of consecutive losing trades (resets on a win). */
  readonly consecutiveLosses: number;
  /** Set when a hard limit auto-engages the switch. null when manually engaged or not engaged. */
  readonly autoEngagedReason: string | null;
}

export type RiskVerdict =
  | { readonly ok: true }
  | { readonly ok: false; readonly reason: string };

export interface TradeResult {
  /** Realized PnL in USD. Negative = loss. */
  readonly realizedPnlUsd: number;
}
