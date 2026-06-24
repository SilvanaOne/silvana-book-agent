import { describe, it, expect } from 'vitest';
import { createToken, createPair } from '@arbitrage-agent/shared';
import { CantexProvider } from './provider.js';
import { toCantexInstrument, isSupportedPair } from './symbol.js';

const cc = createToken('CC', 'Canton Coin', 6, 'canton');
const usdcx = createToken('USDCx', 'USDC (Canton)', 6, 'canton');
const cbtc = createToken('CBTC', 'Canton BTC', 8, 'canton');

const ccUsdcx = createPair(cc, usdcx, 'cantex');
const ccCbtc = createPair(cc, cbtc, 'cantex'); // not a launch pool
const ccUsdcxOneswap = createPair(cc, usdcx, 'oneswap'); // wrong venue

describe('symbol mapping', () => {
  it('maps canonical instruments', () => {
    expect(toCantexInstrument('CC')).toBe('CC');
    expect(toCantexInstrument('USDCx')).toBe('USDCx');
  });

  it('supports only the CC/USDCx launch pool (either order)', () => {
    expect(isSupportedPair(ccUsdcx)).toBe(true);
    expect(isSupportedPair(createPair(usdcx, cc, 'cantex'))).toBe(true);
    expect(isSupportedPair(ccCbtc)).toBe(false);
  });
});

describe('CantexProvider (mock mode)', () => {
  const provider = new CantexProvider({ mode: 'mock', mockJitterPct: 0 });

  it('supports only its venue + launch pool', () => {
    expect(provider.supportsPair(ccUsdcx)).toBe(true);
    expect(provider.supportsPair(ccCbtc)).toBe(false);
    expect(provider.supportsPair(ccUsdcxOneswap)).toBe(false);
  });

  it('returns null for unsupported pairs', async () => {
    expect(await provider.getQuote(ccCbtc)).toBeNull();
    expect(await provider.getQuote(ccUsdcxOneswap)).toBeNull();
  });

  it('returns a fee-spread quote around the mock mid', async () => {
    const q = await provider.getQuote(ccUsdcx);
    expect(q).not.toBeNull();
    expect(q!.buyPrice).toBeCloseTo(0.0198 * 1.003, 9);
    expect(q!.sellPrice).toBeCloseTo(0.0198 * 0.997, 9);
    expect(q!.buyPrice).toBeGreaterThan(q!.sellPrice);
    expect(q!.venueId).toBe('cantex');
  });

  it('mock mid sits below OneSwap → buy Cantex, sell elsewhere', async () => {
    // Cantex mid 0.0198 → ask 0.0198*1.003 = 0.0198594.
    // OneSwap mid 0.02 → bid 0.02*0.997 = 0.01994. Temple bid ~0.0204795.
    // So Cantex is the cheapest place to buy in the 3-way cluster.
    const q = await provider.getQuote(ccUsdcx);
    expect(q!.buyPrice).toBeLessThan(0.0205 * 0.999); // below Temple bid
  });
});

describe('CantexProvider (live mode config)', () => {
  it('throws without an apiKey', () => {
    expect(() => new CantexProvider({ mode: 'live' })).toThrow(/apiKey/);
  });

  it('defaults to live when apiKey present, mock otherwise', async () => {
    expect(new CantexProvider({ apiKey: 'k' }).supportsPair(ccUsdcx)).toBe(true);
    const q = await new CantexProvider().getQuote(ccUsdcx);
    expect(q).not.toBeNull(); // mock, no network
  });
});
