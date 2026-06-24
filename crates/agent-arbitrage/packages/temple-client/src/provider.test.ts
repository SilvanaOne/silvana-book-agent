import { describe, it, expect } from 'vitest';
import { createToken, createPair } from '@arbitrage-agent/shared';
import { TempleProvider } from './provider.js';
import { toTempleSymbol, isSupportedPair } from './symbol.js';

const cc = createToken('CC', 'Canton Coin', 6, 'canton');
const usdcx = createToken('USDCx', 'USDC (Canton)', 6, 'canton');
const cbtc = createToken('CBTC', 'Canton BTC', 8, 'canton');
const usdt = createToken('USDT', 'Tether', 6, 'cex');

const ccUsdcx = createPair(cc, usdcx, 'temple');
const cbtcUsdcx = createPair(cbtc, usdcx, 'temple');
const ccUsdt = createPair(cc, usdt, 'temple'); // unsupported pair
const ccUsdcxOneswap = createPair(cc, usdcx, 'oneswap'); // wrong venue

describe('symbol mapping', () => {
  it('builds Temple market symbols', () => {
    expect(toTempleSymbol(ccUsdcx)).toBe('CC/USDCx');
    expect(toTempleSymbol(cbtcUsdcx)).toBe('CBTC/USDCx');
  });

  it('recognises supported pairs in either order', () => {
    expect(isSupportedPair(ccUsdcx)).toBe(true);
    expect(isSupportedPair(createPair(usdcx, cc, 'temple'))).toBe(true);
    expect(isSupportedPair(cbtcUsdcx)).toBe(true);
    expect(isSupportedPair(ccUsdt)).toBe(false);
  });
});

describe('TempleProvider (mock mode)', () => {
  const provider = new TempleProvider({ mode: 'mock', mockJitterPct: 0 });

  it('supports only its venue + listed pairs', () => {
    expect(provider.supportsPair(ccUsdcx)).toBe(true);
    expect(provider.supportsPair(cbtcUsdcx)).toBe(true);
    expect(provider.supportsPair(ccUsdt)).toBe(false);
    expect(provider.supportsPair(ccUsdcxOneswap)).toBe(false);
  });

  it('returns null for unsupported pairs', async () => {
    expect(await provider.getQuote(ccUsdt)).toBeNull();
    expect(await provider.getQuote(ccUsdcxOneswap)).toBeNull();
  });

  it('returns a tight CLOB bid/ask around the mock mid', async () => {
    const q = await provider.getQuote(ccUsdcx);
    expect(q).not.toBeNull();
    // mid 0.0205, half-spread 0.1%
    expect(q!.buyPrice).toBeCloseTo(0.0205 * 1.001, 9);
    expect(q!.sellPrice).toBeCloseTo(0.0205 * 0.999, 9);
    expect(q!.buyPrice).toBeGreaterThan(q!.sellPrice);
    expect(q!.venueId).toBe('temple');
  });

  it('mock mid sits above OneSwap so a cross-venue spread exists', async () => {
    // OneSwap mock CC/USDCx mid = 0.02, pool fee 0.3% → bid 0.01994.
    // Temple mock CC/USDCx mid = 0.0205 → bid 0.0205*0.999 = 0.0204795.
    // Buy OneSwap ask (0.02006), sell Temple bid (0.0204795) → ~2% spread.
    const q = await provider.getQuote(ccUsdcx);
    const templeBid = q!.sellPrice;
    const oneswapAsk = 0.02 * 1.003;
    expect(templeBid).toBeGreaterThan(oneswapAsk);
  });
});

describe('TempleProvider (live mode config)', () => {
  it('throws without an apiKey', () => {
    expect(() => new TempleProvider({ mode: 'live' })).toThrow(/apiKey/);
  });

  it('defaults to live when apiKey present, mock otherwise', async () => {
    expect(new TempleProvider({ apiKey: 'k' }).supportsPair(ccUsdcx)).toBe(true);
    const q = await new TempleProvider().getQuote(ccUsdcx);
    expect(q).not.toBeNull(); // mock, no network
  });
});
