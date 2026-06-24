import type { SpreadOpportunity } from '@arbitrage-agent/shared';
import { BaseEngineClient } from './base.js';
import type {
  AbortResult,
  DualTradeAccepted,
  DualTradeRequest,
  DualTradeStatus,
  EmergencyStopResult,
  Health,
  SpreadOpportunityDTO,
} from './types.js';

/** Flatten a shared `SpreadOpportunity` into the wire DTO the executor expects. */
export function toOpportunityDTO(opp: SpreadOpportunity): SpreadOpportunityDTO {
  return {
    baseSymbol: opp.pair.base.symbol,
    quoteSymbol: opp.pair.quote.symbol,
    buyVenue: opp.buyFrom.venueId,
    sellVenue: opp.sellTo.venueId,
    buyPrice: opp.buyFrom.buyPrice,
    sellPrice: opp.sellTo.sellPrice,
    spreadPct: opp.spreadPct,
    estimatedProfitUsd: opp.estimatedProfitUsd,
    detectedAt: opp.detectedAt,
  };
}

/**
 * Typed client for the proprietary EXECUTOR engine (two-leg trade execution).
 *
 * Open core calls this; the engine itself is a separate private repo. CEX legs
 * execute SEQUENTIALLY inside the engine (buy → fill-confirm → sell) — see
 * md_docs/12 + md_docs/upgrade-plan.md.
 */
export class ExecutorClient extends BaseEngineClient {
  protected readonly engineName = 'executor';

  /** Submit a two-leg trade. Returns a tradeId to poll. */
  executeDualTrade(req: DualTradeRequest): Promise<DualTradeAccepted> {
    return this.request<DualTradeAccepted>('POST', '/execute-dual-trade', req);
  }

  /** Poll a submitted trade's status + diagnostics. */
  getDualTrade(tradeId: string): Promise<DualTradeStatus> {
    return this.request<DualTradeStatus>('GET', `/execute-dual-trade/${encodeURIComponent(tradeId)}`);
  }

  /** Best-effort abort of an in-flight trade. */
  abortDualTrade(tradeId: string): Promise<AbortResult> {
    return this.request<AbortResult>('POST', `/execute-dual-trade/${encodeURIComponent(tradeId)}/abort`);
  }

  /** Kill-switch: stop accepting new trades and abort all in-flight. */
  emergencyStop(): Promise<EmergencyStopResult> {
    return this.request<EmergencyStopResult>('POST', '/emergency-stop');
  }

  health(): Promise<Health> {
    return this.request<Health>('GET', '/health');
  }
}
