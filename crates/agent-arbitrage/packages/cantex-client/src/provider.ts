/**
 * Cantex read-only venue provider (Canton AMM DEX, by CaviarNine).
 *
 * Open-core scope = QUOTES ONLY for spread detection. Swap execution
 * (`swap`/`swap_and_confirm`, intent secp256k1 signing) lives in the
 * proprietary executor; stable-router v2 lives in the rebalancer. NOT this
 * repo. See md_docs/12 §Sprint 5.
 *
 * Cantex ships a Python-only SDK (Apache-2.0). Rather than spawn a Python
 * subprocess, we port the read-only REST surface (`get_swap_quote`) with
 * native fetch — matching the OneSwap/Temple clients.
 *
 * Two modes:
 *   mock — deterministic synthetic AMM quotes; no credentials. Sprint 5
 *          default. CC/USDCx mock mid (0.0198) is a third distinct value
 *          alongside OneSwap (0.02) and Temple (0.0205) so the scanner
 *          exercises the 3-way USDCx cluster. Mids illustrative.
 *   live — calls the Cantex quote REST with an API key (obtained via the
 *          operator Ed25519 onboarding — out of scope here). REST path/
 *          envelope are best-effort until verified against a live key.
 *
 * AMM pricing: probe the pool in both directions for a reference notional and
 * fold in the pool fee, same as the OneSwap client.
 */

import {
  type Pair,
  type DexQuote,
  type IVenueProvider,
  type VenueId,
  fetchJson,
} from '@arbitrage-agent/shared';
import { toCantexInstrument, isSupportedPair } from './symbol.js';
import type { CantexSwapQuote } from './types.js';

export type CantexMode = 'live' | 'mock';
export type CantexNetwork = 'testnet' | 'mainnet';

export interface CantexProviderConfig {
  /** 'live' requires apiKey. Defaults to 'live' when apiKey is set, else 'mock'. */
  readonly mode?: CantexMode;
  /** API key obtained via operator-key onboarding (live mode). */
  readonly apiKey?: string;
  /** testnet or mainnet. Default 'mainnet' (testnet was unreachable from dev). */
  readonly network?: CantexNetwork;
  /** Override base URL (else derived from network). */
  readonly baseUrl?: string;
  /** Per-request timeout (ms). Default 10000. */
  readonly timeoutMs?: number;
  /** Reference base-token notional used to probe AMM price. Default 100. */
  readonly referenceBaseAmount?: number;
  /** Mock mid prices, keyed canonical 'BASE/QUOTE'. */
  readonly mockMidPrices?: Readonly<Record<string, number>>;
  /** Mock jitter as a fraction of mid. Default 0 (deterministic). */
  readonly mockJitterPct?: number;
}

const NET_BASE_URL: Record<CantexNetwork, string> = {
  testnet: 'https://api.testnet.cantex.io',
  mainnet: 'https://api.cantex.io',
};

/** Cantex pool fee (admin + liquidity), modelled at 0.3% for mock. */
const POOL_FEE = 0.003;

const DEFAULT_MOCK_MIDS: Readonly<Record<string, number>> = {
  'CC/USDCx': 0.0198,
};

export class CantexProvider implements IVenueProvider {
  readonly venueId: VenueId = 'cantex';
  readonly name = 'Cantex';

  private readonly mode: CantexMode;
  private readonly apiKey: string | undefined;
  private readonly baseUrl: string;
  private readonly timeoutMs: number;
  private readonly referenceBaseAmount: number;
  private readonly mockMidPrices: Readonly<Record<string, number>>;
  private readonly mockJitterPct: number;

  constructor(config: CantexProviderConfig = {}) {
    this.apiKey = config.apiKey;
    this.mode = config.mode ?? (config.apiKey ? 'live' : 'mock');
    const net = config.network ?? 'mainnet';
    this.baseUrl = config.baseUrl ?? NET_BASE_URL[net];
    this.timeoutMs = config.timeoutMs ?? 10_000;
    this.referenceBaseAmount = config.referenceBaseAmount ?? 100;
    this.mockMidPrices = config.mockMidPrices ?? DEFAULT_MOCK_MIDS;
    this.mockJitterPct = config.mockJitterPct ?? 0;

    if (this.mode === 'live' && !this.apiKey) {
      throw new Error('CantexProvider: live mode requires an apiKey');
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
      buyPrice: m * (1 + POOL_FEE),
      sellPrice: m * (1 - POOL_FEE),
      timestamp: Date.now(),
    });
  }

  // ── live ────────────────────────────────────────────────────────────────

  private async liveQuote(pair: Pair): Promise<DexQuote | null> {
    const base = toCantexInstrument(pair.base.symbol);
    const quote = toCantexInstrument(pair.quote.symbol);

    const sellOut = await this.fetchBuyAmount(base, quote, this.referenceBaseAmount);
    if (sellOut === null) return null;
    const sellPrice = sellOut / this.referenceBaseAmount; // quote received per base

    const buyOut = await this.fetchBuyAmount(quote, base, sellOut);
    if (buyOut === null || !(buyOut > 0)) return null;
    const buyPrice = sellOut / buyOut; // quote paid per base

    if (!Number.isFinite(buyPrice) || !Number.isFinite(sellPrice)) return null;
    if (buyPrice <= 0 || sellPrice <= 0) return null;

    return Object.freeze({
      venueId: this.venueId,
      buyPrice,
      sellPrice,
      timestamp: Date.now(),
    });
  }

  /**
   * One directional AMM quote via `get_swap_quote`: how much buy-instrument is
   * received for `sellAmount` of sell-instrument. Endpoint shape is best-effort
   * from the SDK surface — verify against a live key before production use.
   */
  private async fetchBuyAmount(
    sellInstrument: string,
    buyInstrument: string,
    sellAmount: number,
  ): Promise<number | null> {
    const headers: Record<string, string> = { 'content-type': 'application/json' };
    if (this.apiKey) headers['authorization'] = `Bearer ${this.apiKey}`;

    const body = await fetchJson<CantexSwapQuote>(`${this.baseUrl}/v1/swap/quote`, {
      method: 'POST',
      headers,
      body: JSON.stringify({
        sell_amount: String(sellAmount),
        sell_instrument: sellInstrument,
        buy_instrument: buyInstrument,
      }),
      timeoutMs: this.timeoutMs,
    });

    const out = Number(body.buy_amount);
    return Number.isFinite(out) && out > 0 ? out : null;
  }
}
