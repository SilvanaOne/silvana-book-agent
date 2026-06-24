import { describe, it, expect } from 'vitest';
import { createToken, createPair } from '@arbitrage-agent/shared';
import { SilvanaProvider } from './provider.js';
import { toSilvanaMarket, isSupportedPair } from './symbol.js';

const cc = createToken('CC', 'Canton Coin', 6, 'canton');
const usdc = createToken('USDC', 'USD Coin (Splice)', 6, 'canton');
const usdcx = createToken('USDCx', 'USDC (Canton)', 6, 'canton');

const ccUsdc = createPair(cc, usdc, 'silvana');
const ccUsdcx = createPair(cc, usdcx, 'silvana'); // wrong stable
const ccUsdcOneswap = createPair(cc, usdc, 'oneswap'); // wrong venue

describe('symbol mapping', () => {
  it('builds hyphenated Silvana market ids', () => {
    expect(toSilvanaMarket(ccUsdc)).toBe('CC-USDC');
  });

  it('supports only CC/USDC', () => {
    expect(isSupportedPair(ccUsdc)).toBe(true);
    expect(isSupportedPair(createPair(usdc, cc, 'silvana'))).toBe(true);
    expect(isSupportedPair(ccUsdcx)).toBe(false);
  });
});

describe('SilvanaProvider (mock mode)', () => {
  const provider = new SilvanaProvider({ mode: 'mock', mockJitterPct: 0 });

  it('supports only its venue + CC/USDC', () => {
    expect(provider.supportsPair(ccUsdc)).toBe(true);
    expect(provider.supportsPair(ccUsdcx)).toBe(false);
    expect(provider.supportsPair(ccUsdcOneswap)).toBe(false);
  });

  it('returns a CLOB bid/ask around the mock mid', async () => {
    const q = await provider.getQuote(ccUsdc);
    expect(q).not.toBeNull();
    expect(q!.buyPrice).toBeCloseTo(0.0206 * 1.001, 9);
    expect(q!.sellPrice).toBeCloseTo(0.0206 * 0.999, 9);
    expect(q!.buyPrice).toBeGreaterThan(q!.sellPrice);
    expect(q!.venueId).toBe('silvana');
  });

  it('returns null for unsupported pairs', async () => {
    expect(await provider.getQuote(ccUsdcx)).toBeNull();
  });
});

describe('SilvanaProvider (live mode)', () => {
  it('throws a clear deferred error', async () => {
    const provider = new SilvanaProvider({ mode: 'live' });
    await expect(provider.getQuote(ccUsdc)).rejects.toThrow(/not yet wired/);
  });
});
