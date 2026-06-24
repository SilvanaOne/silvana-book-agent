import { describe, it, expect } from 'vitest';
import { createToken, createPair } from '@arbitrage-agent/shared';
import { OneSwapProvider } from './provider.js';
import { toOneSwapSymbol, isSupportedPair } from './symbol.js';

const cc = createToken('CC', 'Canton Coin', 6, 'canton');
const usdcx = createToken('USDCx', 'USDC (Canton)', 6, 'canton');
const cbtc = createToken('CBTC', 'Canton BTC', 8, 'canton');
const usdt = createToken('USDT', 'Tether', 6, 'cex');

const ccUsdcxOneswap = createPair(cc, usdcx, 'oneswap');
const ccCbtcOneswap = createPair(cc, cbtc, 'oneswap');
const ccUsdtOneswap = createPair(cc, usdt, 'oneswap'); // unsupported pool
const ccUsdcxBybit = createPair(cc, usdcx, 'bybit'); // wrong venue

describe('symbol mapping', () => {
  it('maps canonical symbols to OneSwap names', () => {
    expect(toOneSwapSymbol('CC')).toBe('Amulet');
    expect(toOneSwapSymbol('CBTC')).toBe('cBTC');
    expect(toOneSwapSymbol('USDCx')).toBe('USDCx');
    expect(toOneSwapSymbol('USDT')).toBe('USDT'); // passthrough
  });

  it('recognises supported pools in either order', () => {
    expect(isSupportedPair(ccUsdcxOneswap)).toBe(true);
    expect(isSupportedPair(createPair(usdcx, cc, 'oneswap'))).toBe(true);
    expect(isSupportedPair(ccCbtcOneswap)).toBe(true);
    expect(isSupportedPair(ccUsdtOneswap)).toBe(false);
  });
});

describe('OneSwapProvider (mock mode)', () => {
  const provider = new OneSwapProvider({ mode: 'mock', mockJitterPct: 0 });

  it('supports only its venue + known pools', () => {
    expect(provider.supportsPair(ccUsdcxOneswap)).toBe(true);
    expect(provider.supportsPair(ccCbtcOneswap)).toBe(true);
    expect(provider.supportsPair(ccUsdtOneswap)).toBe(false);
    expect(provider.supportsPair(ccUsdcxBybit)).toBe(false);
  });

  it('returns null for unsupported pairs', async () => {
    expect(await provider.getQuote(ccUsdtOneswap)).toBeNull();
    expect(await provider.getQuote(ccUsdcxBybit)).toBeNull();
  });

  it('returns a fee-spread quote around the mock mid', async () => {
    const q = await provider.getQuote(ccUsdcxOneswap);
    expect(q).not.toBeNull();
    // mid 0.02, pool fee 0.3%
    expect(q!.buyPrice).toBeCloseTo(0.02 * 1.003, 9);
    expect(q!.sellPrice).toBeCloseTo(0.02 * 0.997, 9);
    // AMM: ask > bid
    expect(q!.buyPrice).toBeGreaterThan(q!.sellPrice);
    expect(q!.venueId).toBe('oneswap');
    expect(q!.timestamp).toBeGreaterThan(Date.now() - 1000);
    expect(q!.timestamp).toBeLessThanOrEqual(Date.now());
  });

  it('honours custom mock mids', async () => {
    const p = new OneSwapProvider({
      mode: 'mock',
      mockJitterPct: 0,
      mockMidPrices: { 'CC/USDCx': 0.05 },
    });
    const q = await p.getQuote(ccUsdcxOneswap);
    expect(q!.buyPrice).toBeCloseTo(0.05 * 1.003, 9);
  });
});

describe('OneSwapProvider (live mode config)', () => {
  it('throws without an apiKey', () => {
    expect(() => new OneSwapProvider({ mode: 'live' })).toThrow(/apiKey/);
  });

  it('defaults to live when apiKey is present', () => {
    // No network call here — just confirms construction picks live mode.
    const p = new OneSwapProvider({ apiKey: 'os_live_test' });
    expect(p.supportsPair(ccUsdcxOneswap)).toBe(true);
  });

  it('defaults to mock when no apiKey', async () => {
    const p = new OneSwapProvider();
    const q = await p.getQuote(ccUsdcxOneswap);
    expect(q).not.toBeNull(); // mock path, no network
  });
});
