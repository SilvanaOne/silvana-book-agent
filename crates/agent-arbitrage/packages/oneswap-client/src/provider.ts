/**
 * OneSwap read-only venue provider (Canton AMM DEX).
 *
 * Open-core scope = QUOTES ONLY, for spread detection. Swap execution
 * (`swaps.create`, deposit detection, completion) is NOT here — it lives in
 * the proprietary executor (separate repo). The intent-deposit routing lives
 * in the proprietary rebalancer. See md_docs/12-component-split §Sprint 3.
 *
 * Two modes:
 *   mock — deterministic synthetic AMM quotes; no credentials needed. This is
 *          the Sprint 3 default, mirroring silvana-host standalone-dev. Lets
 *          the scanner exercise the M3 (OneSwap) leg end-to-end without a
 *          devnet key.
 *   live — calls the OneSwap REST quote endpoint with an `os_live_...` API key.
 *          The exact REST path/envelope is best-effort from the SDK's
 *          documented surface and MUST be verified against a live devnet key
 *          before relying on it (we have none yet).
 *
 * AMM pricing: an AMM has no resting order book, so "bid/ask" are derived by
 * probing the pool in both directions for a reference notional and folding in
 * the pool fee. buyPrice = quote paid per base, sellPrice = quote received
 * per base. The gap between them reflects pool fee + price impact.
 */

import {
  type Pair,
  type DexQuote,
  type IVenueProvider,
  type VenueId,
  fetchJson,
} from '@arbitrage-agent/shared';
import { toOneSwapSymbol, isSupportedPair } from './symbol.js';
import type { OneSwapQuoteResponse } from './types.js';

export type OneSwapMode = 'live' | 'mock';
export type OneSwapEnvironment = 'devnet' | 'mainnet';

export interface OneSwapProviderConfig {
  /** 'live' requires apiKey. Defaults to 'live' when apiKey is set, else 'mock'. */
  readonly mode?: OneSwapMode;
  /** `os_live_...` developer key (live mode only). */
  readonly apiKey?: string;
  /** Devnet or mainnet. Default 'devnet'. */
  readonly environment?: OneSwapEnvironment;
  /** Override base URL (else derived from environment). */
  readonly baseUrl?: string;
  /** Per-request timeout (ms). Default 10000. */
  readonly timeoutMs?: number;
  /** Reference base-token notional used to probe AMM price. Default 100. */
  readonly referenceBaseAmount?: number;
  /** Mock mid prices, keyed canonical 'BASE/QUOTE'. */
  readonly mockMidPrices?: Readonly<Record<string, number>>;
  /** Mock jitter as a fraction of mid (e.g. 0.002 = ±0.2%). Default 0 (deterministic). */
  readonly mockJitterPct?: number;
}

const ENV_BASE_URL: Record<OneSwapEnvironment, string> = {
  devnet: 'https://devnet.oneswap.cc',
  mainnet: 'https://oneswap.cc',
};

/** OneSwap pool fee — 0.3% per swap (reference-oneswap-sdk memory). */
const POOL_FEE = 0.003;

const DEFAULT_MOCK_MIDS: Readonly<Record<string, number>> = {
  'CC/USDCx': 0.02,
  'CC/CBTC': 0.0000003,
};

export class OneSwapProvider implements IVenueProvider {
  readonly venueId: VenueId = 'oneswap';
  readonly name = 'OneSwap';

  private readonly mode: OneSwapMode;
  private readonly apiKey: string | undefined;
  private readonly baseUrl: string;
  private readonly timeoutMs: number;
  private readonly referenceBaseAmount: number;
  private readonly mockMidPrices: Readonly<Record<string, number>>;
  private readonly mockJitterPct: number;

  constructor(config: OneSwapProviderConfig = {}) {
    this.apiKey = config.apiKey;
    this.mode = config.mode ?? (config.apiKey ? 'live' : 'mock');
    const env = config.environment ?? 'devnet';
    this.baseUrl = config.baseUrl ?? ENV_BASE_URL[env];
    this.timeoutMs = config.timeoutMs ?? 10_000;
    this.referenceBaseAmount = config.referenceBaseAmount ?? 100;
    this.mockMidPrices = config.mockMidPrices ?? DEFAULT_MOCK_MIDS;
    this.mockJitterPct = config.mockJitterPct ?? 0;

    if (this.mode === 'live' && !this.apiKey) {
      throw new Error('OneSwapProvider: live mode requires an apiKey (os_live_...)');
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

    // Slow deterministic oscillation so the scanner sees movement vs the CEX
    // legs without being random (reproducible within a ~minute window).
    const jitter =
      this.mockJitterPct > 0
        ? 1 + this.mockJitterPct * Math.sin(Date.now() / 60_000)
        : 1;
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
    const base = toOneSwapSymbol(pair.base.symbol);
    const quote = toOneSwapSymbol(pair.quote.symbol);

    // Probe both directions: sell `referenceBaseAmount` base for quote
    // (→ sellPrice), and buy back base with that quote (→ buyPrice).
    const sellOut = await this.fetchOutput(base, quote, this.referenceBaseAmount);
    if (sellOut === null) return null;
    const sellPrice = sellOut / this.referenceBaseAmount; // quote received per base

    const buyOut = await this.fetchOutput(quote, base, sellOut);
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
   * One directional AMM quote: how much `to` is received for `amount` of `from`.
   *
   * Endpoint shape is best-effort from the SDK's documented `quotes.get` —
   * verify against live devnet docs before production use.
   */
  private async fetchOutput(from: string, to: string, amount: number): Promise<number | null> {
    const headers: Record<string, string> = { 'content-type': 'application/json' };
    if (this.apiKey) headers['authorization'] = `Bearer ${this.apiKey}`;

    const body = await fetchJson<OneSwapQuoteResponse>(`${this.baseUrl}/api/sdk/quotes`, {
      method: 'POST',
      headers,
      body: JSON.stringify({ from, to, amount: String(amount) }),
      timeoutMs: this.timeoutMs,
    });

    const out = Number(body.outputAmount);
    return Number.isFinite(out) && out > 0 ? out : null;
  }
}
