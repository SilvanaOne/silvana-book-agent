/**
 * Advisory features (md_docs/13 #3/#4/#5). Each returns a SUGGESTION; nothing is
 * applied. Default implementation is deterministic (heuristic provider); a real
 * LLM provider would phrase the same structured result more naturally.
 */

import type { ApprovalManager } from './approval.js';
import type {
  ConfigDiffEntry,
  ConfigSuggestResult,
  PostmortemResult,
  SymbolResolveResult,
  TradeRecord,
} from './types.js';

// ── Feature #4: Trade post-mortem ──────────────────────────────────────────
export function tradePostmortem(trade: TradeRecord): PostmortemResult {
  const route = `${trade.buyVenueId} → ${trade.sellVenueId}`;
  if (trade.status === 'SUCCESS') {
    return {
      whatHappened: `Two-leg trade on ${trade.ticker} (${route}) executed; both legs filled.`,
      whyItFailed: 'n/a — trade succeeded.',
      suggestedActions: [],
      confidence: 0.9,
    };
  }
  if (trade.status === 'PARTIAL') {
    return {
      whatHappened: `Buy leg filled on ${trade.buyVenueId} but the sell leg on ${trade.sellVenueId} did not (${trade.ticker}).`,
      whyItFailed: trade.failReason ?? 'sell leg rejected or unfilled after the buy confirmed.',
      suggestedActions: [
        { description: `Hedge the bought ${trade.ticker} base (open a short perp) until you can re-sell, or retry the sell leg.`, requiresApproval: true },
        { description: 'Lower trade size or raise the safety buffer so the sell leg fits venue minimums/liquidity.', requiresApproval: true },
      ],
      confidence: 0.8,
    };
  }
  if (trade.status === 'SKIPPED') {
    return {
      whatHappened: `Trade on ${trade.ticker} (${route}) was skipped before any order.`,
      whyItFailed: trade.failReason ?? 'pre-flight rejected it (spread, balance, or dry-run).',
      suggestedActions: [{ description: 'Check the fail reason; if it is balance, top up the buy-venue quote or lower trade size.', requiresApproval: false }],
      confidence: 0.85,
    };
  }
  return {
    whatHappened: `Trade on ${trade.ticker} (${route}) failed.`,
    whyItFailed: trade.failReason ?? 'the buy leg threw or never filled — no capital moved.',
    suggestedActions: [{ description: 'Inspect the buy venue health / API errors; capital did not move, safe to retry.', requiresApproval: false }],
    confidence: 0.75,
  };
}

// ── Feature #5: Natural-language config ─────────────────────────────────────
const VENUE_TOKENS = ['bybit', 'kucoin', 'okx', 'mexc', 'weex', 'binance', 'bitget', 'toobit', 'hyperliquid', 'temple', 'oneswap', 'cantex', 'silvana'];

export function configSuggest(naturalLanguage: string, currentConfig: Record<string, unknown>, approvals: ApprovalManager): ConfigSuggestResult {
  const text = naturalLanguage.toLowerCase();
  const diff: ConfigDiffEntry[] = [];
  const sideEffects: string[] = [];

  // "min spread to 30 bps" / "minimum spread 0.3%"
  const bps = text.match(/(?:min(?:imum)?\s+spread).*?(\d+(?:\.\d+)?)\s*bps/);
  const pct = text.match(/(?:min(?:imum)?\s+spread).*?(\d+(?:\.\d+)?)\s*%/);
  if (bps?.[1]) {
    const newPct = Number(bps[1]) / 100;
    diff.push({ path: 'tradeConfig.targetSpreadPercent', old: (currentConfig['targetSpreadPercent'] ?? null), new: newPct });
    sideEffects.push('Detection threshold changed — fewer/more opportunities will be flagged.');
  } else if (pct?.[1]) {
    diff.push({ path: 'tradeConfig.targetSpreadPercent', old: (currentConfig['targetSpreadPercent'] ?? null), new: Number(pct[1]) });
    sideEffects.push('Detection threshold changed — fewer/more opportunities will be flagged.');
  }

  // "disable X" / "enable X"
  const toggle = text.match(/\b(enable|disable)\b\s+([a-z]+)/);
  if (toggle?.[2] && VENUE_TOKENS.includes(toggle[2])) {
    const enabled = toggle[1] === 'enable';
    diff.push({ path: `venues.${toggle[2]}.enabled`, old: null, new: enabled });
    if (toggle[2] === 'hyperliquid' && !enabled) sideEffects.push('Hyperliquid is the failsafe hedge venue — disabling narrows hedge fallback.');
  }

  const approval = approvals.create('config-diff', diff);
  return {
    proposedDiff: diff,
    explanation: diff.length
      ? `Interpreted "${naturalLanguage}" as ${diff.length} change(s). Review and confirm to apply.`
      : `Could not derive a config change from "${naturalLanguage}". Try e.g. "set min spread to 30 bps" or "disable hyperliquid".`,
    sideEffects,
    confirmationRequired: true,
    approvalId: approval.id,
  };
}

// ── Feature #3: Symbol resolver ─────────────────────────────────────────────
const SYMBOL_REGISTRY: Record<string, SymbolResolveResult['candidates']> = {
  cc: [
    { canonicalId: 'CANTON:CC', displayName: 'Canton Coin (Canton Network)', confidence: 0.97, attributes: { issuer: 'Canton', chain: 'canton' } },
  ],
  usdc: [
    { canonicalId: 'ETHEREUM:0xa0b8...:USDC', displayName: 'USD Coin (Circle, Ethereum)', confidence: 0.9, attributes: { issuer: 'Circle', chain: 'ethereum' } },
    { canonicalId: 'CANTON:USDCx', displayName: 'USDCx (Canton, Circle-backed)', confidence: 0.75, attributes: { issuer: 'Circle', chain: 'canton' } },
  ],
  cbtc: [
    { canonicalId: 'CANTON:cBTC', displayName: 'cBTC (BitSafe, 1:1 BTC)', confidence: 0.95, attributes: { issuer: 'BitSafe', chain: 'canton' } },
  ],
};

export function resolveSymbol(query: string, _context?: Record<string, unknown>): SymbolResolveResult {
  const key = query.trim().toLowerCase();
  const candidates = SYMBOL_REGISTRY[key] ?? [];
  return {
    candidates,
    requiresOperatorChoice: candidates.length !== 1,
    explanation: candidates.length === 0
      ? `No known mapping for "${query}". Operator must add it to the token registry manually.`
      : candidates.length === 1
        ? `Single confident mapping for "${query}".`
        : `"${query}" is ambiguous across chains/venues — operator must choose.`,
  };
}
