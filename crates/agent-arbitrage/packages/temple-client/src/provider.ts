/**
 * Temple read-only venue provider (Canton privacy CLOB DEX).
 *
 * Open-core scope = MARKET DATA ONLY (ticker/orderbook) for spread detection.
 * Order placement (`createOrderRequest`), deposit, and withdraw live in the
 * proprietary executor / rebalancer (separate repos). See md_docs/12 §Sprint 4.
 *
 * Two modes (mirroring oneswap-client):
 *   mock — deterministic synthetic CLOB top-of-book; no credentials. Sprint 4
 *          default. The mock CC/USDCx mid is set slightly above OneSwap's so
 *          the scanner exercises the first cross-venue Canton spread (M3↔M4,
 *          USDCx cluster). Mids are illustrative, not market truth.
 *   live — calls Temple's market-data REST with an API key. The exact REST
 *          path/envelope is best-effort from the SDK's documented surface and
 *          MUST be verified against a live key (we have none — KYC pending).
 *
 * Temple is a CLOB, so bid/ask are real top-of-book levels (a tighter spread
 * than an AMM's fee-derived gap).
 */

import {
  type Pair,
  type DexQuote,
  type IVenueProvider,
  type VenueId,
  fetchJson,
} from '@arbitrage-agent/shared';
import { toTempleSymbol, isSupportedPair } from './symbol.js';
import type { TempleTicker } from './types.js';

export type TempleMode = 'live' | 'mock';
export type TempleNetwork = 'testnet' | 'mainnet';

export interface TempleProviderConfig {
  /** 'live' requires apiKey. Defaults to 'live' when apiKey is set, else 'mock'. */
  readonly mode?: TempleMode;
  /** API key from app.templedigitalgroup.com (live mode). */
  readonly apiKey?: string;
  /** testnet or mainnet. Default 'testnet'. */
  readonly network?: TempleNetwork;
  /** Override base URL (else derived from network). */
  readonly baseUrl?: string;
  /** Per-request timeout (ms). Default 10000. */
  readonly timeoutMs?: number;
  /** Mock mid prices, keyed canonical 'BASE/QUOTE'. */
  readonly mockMidPrices?: Readonly<Record<string, number>>;
  /** Mock half-spread as a fraction of mid (CLOB tightness). Default 0.001 (±0.1%). */
  readonly mockHalfSpreadPct?: number;
  /** Mock jitter as a fraction of mid. Default 0 (deterministic). */
  readonly mockJitterPct?: number;
}

const NET_BASE_URL: Record<TempleNetwork, string> = {
  testnet: 'https://api-testnet.templedigitalgroup.com',
  mainnet: 'https://api.templedigitalgroup.com',
};

/**
 * Mock mids. CC/USDCx sits ~2.5% above OneSwap's mock mid (0.02) so the
 * scanner detects a real cross-venue spread once both Canton venues are on.
 */
const DEFAULT_MOCK_MIDS: Readonly<Record<string, number>> = {
  'CC/USDCx': 0.0205,
  'CBTC/USDCx': 95000,
};

export class TempleProvider implements IVenueProvider {
  readonly venueId: VenueId = 'temple';
  readonly name = 'Temple';

  private readonly mode: TempleMode;
  private readonly apiKey: string | undefined;
  private readonly baseUrl: string;
  private readonly timeoutMs: number;
  private readonly mockMidPrices: Readonly<Record<string, number>>;
  private readonly mockHalfSpreadPct: number;
  private readonly mockJitterPct: number;

  constructor(config: TempleProviderConfig = {}) {
    this.apiKey = config.apiKey;
    this.mode = config.mode ?? (config.apiKey ? 'live' : 'mock');
    const net = config.network ?? 'testnet';
    this.baseUrl = config.baseUrl ?? NET_BASE_URL[net];
    this.timeoutMs = config.timeoutMs ?? 10_000;
    this.mockMidPrices = config.mockMidPrices ?? DEFAULT_MOCK_MIDS;
    this.mockHalfSpreadPct = config.mockHalfSpreadPct ?? 0.001;
    this.mockJitterPct = config.mockJitterPct ?? 0;

    if (this.mode === 'live' && !this.apiKey) {
      throw new Error('TempleProvider: live mode requires an apiKey');
    }
  }

  supportsPair(pair: Pair): boolean {
    return pair.venueId === this.venueId && isSupportedPair(pair);
  }

  async getQuote(pair: Pair): Promise<DexQuote | null> {
    if (!this.supportsPair(pair)) return null;
    return this.mode === 'mock' ? this.mockQuote(pair) : this.liveQuote(pair);
  }

  // ── mock ────────────────────────────────────────────────────────────────

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

  // ── live ────────────────────────────────────────────────────────────────

  private async liveQuote(pair: Pair): Promise<DexQuote | null> {
    const symbol = toTempleSymbol(pair);
    const headers: Record<string, string> = {};
    if (this.apiKey) headers['x-api-key'] = this.apiKey;

    // Best-effort REST path from the SDK's getTicker surface — verify against
    // a live API key before production use.
    const ticker = await fetchJson<TempleTicker>(
      `${this.baseUrl}/api/v1/ticker?symbol=${encodeURIComponent(symbol)}`,
      { headers, timeoutMs: this.timeoutMs },
    );

    const ask = Number(ticker.ask ?? ticker.last);
    const bid = Number(ticker.bid ?? ticker.last);
    if (!Number.isFinite(ask) || !Number.isFinite(bid) || ask <= 0 || bid <= 0) return null;

    return Object.freeze({
      venueId: this.venueId,
      buyPrice: ask,
      sellPrice: bid,
      timestamp: Date.now(),
    });
  }
}
