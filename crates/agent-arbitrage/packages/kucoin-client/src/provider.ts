/**
 * KuCoin read-only venue provider.
 *
 * Reads public spot level-1 orderbook (best bid/ask) via KuCoin REST.
 * No auth required for public market data.
 *
 * Trade execution lives in proprietary executor (separate repo).
 * See md_docs/12-component-split-open-vs-proprietary.md.
 */

import {
  type Pair,
  type DexQuote,
  type IVenueProvider,
  type VenueId,
  fetchJson,
} from '@arbitrage-agent/shared';
import { pairToKucoinSymbol } from './symbol.js';
import type { KucoinEnvelope, KucoinLevel1Ticker } from './types.js';

export interface KucoinProviderConfig {
  /** Base URL. Default mainnet `https://api.kucoin.com`. */
  readonly baseUrl?: string;
  /** Per-request timeout (ms). Default 10000. */
  readonly timeoutMs?: number;
}

const DEFAULT_BASE_URL = 'https://api.kucoin.com';

export class KucoinProvider implements IVenueProvider {
  readonly venueId: VenueId = 'kucoin';
  readonly name = 'KuCoin';

  private readonly baseUrl: string;
  private readonly timeoutMs: number;

  constructor(config: KucoinProviderConfig = {}) {
    this.baseUrl = config.baseUrl ?? DEFAULT_BASE_URL;
    this.timeoutMs = config.timeoutMs ?? 10_000;
  }

  supportsPair(pair: Pair): boolean {
    return pair.venueId === this.venueId;
  }

  async getQuote(pair: Pair): Promise<DexQuote | null> {
    if (!this.supportsPair(pair)) return null;

    const symbol = pairToKucoinSymbol(pair);
    const url = `${this.baseUrl}/api/v1/market/orderbook/level1?symbol=${symbol}`;

    const body = await fetchJson<KucoinEnvelope<KucoinLevel1Ticker>>(url, {
      timeoutMs: this.timeoutMs,
    });

    if (body.code !== '200000') {
      throw new Error(`KuCoin ${symbol}: code=${body.code} ${body.msg ?? ''}`);
    }

    const ticker = body.data;
    if (!ticker) {
      return null;
    }

    const ask = Number(ticker.bestAsk);
    const bid = Number(ticker.bestBid);
    if (!Number.isFinite(ask) || !Number.isFinite(bid) || ask <= 0 || bid <= 0) {
      return null;
    }

    // timestamp = our observation time. KuCoin's `ticker.time` is "last
    // orderbook update" which can be many seconds stale on low-volume
    // pairs even when the bid/ask are still valid. We want freshness from
    // our perspective for the risk module's stale-quote check.
    return Object.freeze({
      venueId: this.venueId,
      buyPrice: ask,
      sellPrice: bid,
      timestamp: Date.now(),
    });
  }
}
