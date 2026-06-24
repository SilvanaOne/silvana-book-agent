/**
 * Silvana Book read-only venue provider (Canton privacy CLOB).
 *
 * Open-core scope = READ-ONLY PRICING for spread detection. Order submission
 * (submitOrder), DvP settlement, and withdraw routing live in the proprietary
 * executor/rebalancer when Silvana is live (credit_limit unblocked). NOT this
 * repo. See md_docs/12 §Sprint 6.
 *
 * Silvana is a gRPC service (`@silvana-one/orderbook`, 5 services incl.
 * Pricing) fronted by the silvana-host runtime. Unlike the REST venues, its
 * read feed is `PricingClient.getPrice(market)` over gRPC — which needs the
 * SDK + devnet onboarding we do not have yet.
 *
 * Two modes:
 *   mock — deterministic synthetic CC/USDC top-of-book; no credentials. Sprint 6
 *          default. Mid is set distinct from the USDCx cluster so that, once
 *          Sprint 7's cross-cluster detector lands, a CC-triangulation spread
 *          emerges between the USDC and USDCx clusters. Mid illustrative.
 *   live — gRPC PricingClient via silvana-host. Deferred until onboarding;
 *          throws a clear error (mirrors silvana-host production mode), so the
 *          read pipeline degrades gracefully rather than guessing a wire format.
 */

import {
  type Pair,
  type DexQuote,
  type IVenueProvider,
  type VenueId,
} from '@arbitrage-agent/shared';
import { isSupportedPair } from './symbol.js';

export type SilvanaMode = 'live' | 'mock';

export interface SilvanaProviderConfig {
  /** Default 'mock'. 'live' is deferred (gRPC PricingClient + onboarding). */
  readonly mode?: SilvanaMode;
  /** Devnet/mainnet gRPC endpoint (live mode, when wired). */
  readonly rpcUrl?: string;
  /** Mock mid prices, keyed canonical 'BASE/QUOTE'. */
  readonly mockMidPrices?: Readonly<Record<string, number>>;
  /** Mock half-spread as a fraction of mid (CLOB tightness). Default 0.001 (±0.1%). */
  readonly mockHalfSpreadPct?: number;
  /** Mock jitter as a fraction of mid. Default 0 (deterministic). */
  readonly mockJitterPct?: number;
}

/**
 * Mock mids. CC/USDC sits a touch above the USDCx cluster's CC price so the
 * Sprint 7 cross-cluster detector finds a CC-triangulation spread (buy CC in
 * the cheaper USDCx cluster, sell CC on Silvana for USDC, net of conversion).
 */
const DEFAULT_MOCK_MIDS: Readonly<Record<string, number>> = {
  'CC/USDC': 0.0206,
};

export class SilvanaProvider implements IVenueProvider {
  readonly venueId: VenueId = 'silvana';
  readonly name = 'Silvana Book';

  private readonly mode: SilvanaMode;
  private readonly mockMidPrices: Readonly<Record<string, number>>;
  private readonly mockHalfSpreadPct: number;
  private readonly mockJitterPct: number;

  constructor(config: SilvanaProviderConfig = {}) {
    this.mode = config.mode ?? 'mock';
    this.mockMidPrices = config.mockMidPrices ?? DEFAULT_MOCK_MIDS;
    this.mockHalfSpreadPct = config.mockHalfSpreadPct ?? 0.001;
    this.mockJitterPct = config.mockJitterPct ?? 0;
  }

  supportsPair(pair: Pair): boolean {
    return pair.venueId === this.venueId && isSupportedPair(pair);
  }

  async getQuote(pair: Pair): Promise<DexQuote | null> {
    if (!this.supportsPair(pair)) return null;
    if (this.mode === 'live') {
      throw new Error(
        'Silvana live pricing not yet wired — requires @silvana-one/orderbook gRPC ' +
          'PricingClient + devnet onboarding (deferred until credit_limit unblocked). ' +
          'Use SILVANA_MODE=mock for read-only development.',
      );
    }
    return this.mockQuote(pair);
  }

  private mockQuote(pair: Pair): DexQuote | null {
    const key = `${pair.base.symbol}/${pair.quote.symbol}`;
    const mid = this.mockMidPrices[key];
    if (mid === undefined || !(mid > 0)) return null;

    const jitter =
      this.mockJitterPct > 0 ? 1 + this.mockJitterPct * Math.sin(Date.now() / 60_000) : 1;
    const m = mid * jitter;

    return Object.freeze({
      venueId: this.venueId,
      buyPrice: m * (1 + this.mockHalfSpreadPct), // ask
      sellPrice: m * (1 - this.mockHalfSpreadPct), // bid
      timestamp: Date.now(),
    });
  }
}
