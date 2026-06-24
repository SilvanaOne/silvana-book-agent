/**
 * @arbitrage-agent/ai-assistant-core — advisory AI layer (open core, BSL).
 *
 * ADVISORY ONLY: every output is a suggestion the operator confirms; this package
 * never trades, signs, holds keys, or mutates config. Deterministic by default
 * (heuristic provider, no API key); real LLM providers are config-only swaps.
 * No Telegram channel. See md_docs/13-ai-assistant-expansion.md.
 */

export * from './types.js';
export { makeProvider, type LlmProvider, type Prompt } from './providers.js';
export { ApprovalManager, type PendingApproval } from './approval.js';
export { CostGuard, type GuardDecision } from './cost.js';
export { tradePostmortem, configSuggest, resolveSymbol } from './features.js';
