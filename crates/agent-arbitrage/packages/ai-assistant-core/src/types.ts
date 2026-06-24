/**
 * Types for the advisory AI layer. ADVISORY ONLY — every output is a suggestion
 * the operator must confirm; nothing here trades, signs, or mutates config.
 * See md_docs/13-ai-assistant-expansion.md.
 */

export type ProviderName = 'heuristic' | 'anthropic' | 'openai' | 'ollama';

/** A per-trade execution summary (mirrors open-core TradeExecution) passed in by
 * the caller — the AI service holds NO DB connection and NO secrets. */
export interface TradeRecord {
  readonly tradeId?: string;
  readonly ticker: string;
  readonly buyVenueId: string;
  readonly sellVenueId: string;
  readonly status: string; // SUCCESS | PARTIAL | FAILED | SKIPPED
  readonly partialExecution?: boolean;
  readonly profitUsdtDiff?: number | string | null;
  readonly spreadBps?: number | null;
  readonly legBuyStatus?: string | null;
  readonly legSellStatus?: string | null;
  readonly failReason?: string | null;
}

export interface SuggestedAction {
  readonly description: string;
  readonly configDiff?: readonly ConfigDiffEntry[];
  readonly requiresApproval: boolean;
}

export interface PostmortemResult {
  readonly whatHappened: string;
  readonly whyItFailed: string;
  readonly suggestedActions: readonly SuggestedAction[];
  readonly confidence: number;
}

export interface ConfigDiffEntry {
  readonly path: string;
  readonly old: unknown;
  readonly new: unknown;
}

export interface ConfigSuggestResult {
  readonly proposedDiff: readonly ConfigDiffEntry[];
  readonly explanation: string;
  readonly sideEffects: readonly string[];
  readonly confirmationRequired: true;
  /** Id of the pending approval the operator must confirm before applying. */
  readonly approvalId: string;
}

export interface SymbolCandidate {
  readonly canonicalId: string;
  readonly displayName: string;
  readonly confidence: number;
  readonly attributes: Record<string, string>;
}

export interface SymbolResolveResult {
  readonly candidates: readonly SymbolCandidate[];
  readonly requiresOperatorChoice: boolean;
  readonly explanation: string;
}
