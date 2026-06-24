/**
 * Bybit read-only venue provider.
 *
 * Reads public spot tickers (best bid/ask) via Bybit V5 REST.
 * No auth required for public market data.
 *
 * Trade execution is NOT here — lives in proprietary executor (separate repo).
 * See md_docs/12-component-split-open-vs-proprietary.md.
 */

import {
  type Pair,
  type DexQuote,
  type IVenueProvider,
  type VenueId,
  fetchJson,
} from '@arbitrage-agent/shared';
import { pairToBybitSymbol } from './symbol.js';
import type { BybitV5Envelope, BybitSpotTickersResult } from './types.js';

export interface BybitProviderConfig {
  /** Base URL. Default mainnet `https://api.bybit.com`. */
  readonly baseUrl?: string;
  /** Per-request timeout (ms). Default 10000. */
  readonly timeoutMs?: number;
}

const DEFAULT_BASE_URL = 'https://api.bybit.com';

export class BybitProvider implements IVenueProvider {
  readonly venueId: VenueId = 'bybit';
  readonly name = 'Bybit';

  private readonly baseUrl: string;
  private readonly timeoutMs: number;

  constructor(config: BybitProviderConfig = {}) {
    this.baseUrl = config.baseUrl ?? DEFAULT_BASE_URL;
    this.timeoutMs = config.timeoutMs ?? 10_000;
  }

  supportsPair(pair: Pair): boolean {
    return pair.venueId === this.venueId;
  }

  async getQuote(pair: Pair): Promise<DexQuote | null> {
    if (!this.supportsPair(pair)) return null;

    const symbol = pairToBybitSymbol(pair);
    const url = `${this.baseUrl}/v5/market/tickers?category=spot&symbol=${symbol}`;

    const body = await fetchJson<BybitV5Envelope<BybitSpotTickersResult>>(url, {
      timeoutMs: this.timeoutMs,
    });

    if (body.retCode !== 0) {
      throw new Error(`Bybit ${symbol}: retCode=${body.retCode} ${body.retMsg}`);
    }

    const ticker = body.result.list[0];
    if (!ticker || ticker.symbol !== symbol) {
      return null;
    }

    const ask = Number(ticker.ask1Price);
    const bid = Number(ticker.bid1Price);
    if (!Number.isFinite(ask) || !Number.isFinite(bid) || ask <= 0 || bid <= 0) {
      return null;
    }

    // timestamp = our observation time (not venue server time), so the
    // risk module's stale-quote check measures actual freshness from our
    // perspective regardless of venue clock drift or orderbook lull on
    // low-volume pairs.
    return Object.freeze({
      venueId: this.venueId,
      buyPrice: ask,
      sellPrice: bid,
      timestamp: Date.now(),
    });
  }
}
