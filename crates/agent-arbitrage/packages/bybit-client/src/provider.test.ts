import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { createToken, createPair } from '@arbitrage-agent/shared';
import { BybitProvider } from './provider.js';
import { pairToBybitSymbol } from './symbol.js';

const CC = createToken('CC', 'Canton Coin', 6, 'canton');
const USDT = createToken('USDT', 'Tether USD', 6, 'cex');
const bybitPair = createPair(CC, USDT, 'bybit');
const kucoinPair = createPair(CC, USDT, 'kucoin');

function mockFetch(responseBody: unknown, options: { status?: number; ok?: boolean } = {}): void {
  const status = options.status ?? 200;
  const ok = options.ok ?? (status >= 200 && status < 300);
  vi.stubGlobal(
    'fetch',
    vi.fn(async () =>
      Promise.resolve({
        ok,
        status,
        text: async () => JSON.stringify(responseBody),
      } as Response),
    ),
  );
}

describe('pairToBybitSymbol', () => {
  it('concatenates base+quote uppercase', () => {
    expect(pairToBybitSymbol(bybitPair)).toBe('CCUSDT');
  });
});

describe('BybitProvider', () => {
  beforeEach(() => {
    vi.unstubAllGlobals();
  });
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('supportsPair returns true for venueId=bybit, false otherwise', () => {
    const provider = new BybitProvider();
    expect(provider.supportsPair(bybitPair)).toBe(true);
    expect(provider.supportsPair(kucoinPair)).toBe(false);
  });

  it('getQuote returns null when pair is not supported', async () => {
    const provider = new BybitProvider();
    const result = await provider.getQuote(kucoinPair);
    expect(result).toBeNull();
  });

  it('getQuote maps ask1Price → buyPrice and bid1Price → sellPrice', async () => {
    mockFetch({
      retCode: 0,
      retMsg: 'OK',
      result: {
        category: 'spot',
        list: [
          {
            symbol: 'CCUSDT',
            bid1Price: '0.15',
            ask1Price: '0.16',
            lastPrice: '0.155',
          },
        ],
      },
      time: 1730000000000,
    });

    const provider = new BybitProvider();
    const quote = await provider.getQuote(bybitPair);

    expect(quote).not.toBeNull();
    expect(quote!.venueId).toBe('bybit');
    expect(quote!.buyPrice).toBe(0.16);
    expect(quote!.sellPrice).toBe(0.15);
    // timestamp is our observation time (Date.now() at parse time), not venue's body.time
    expect(quote!.timestamp).toBeGreaterThan(Date.now() - 1000);
    expect(quote!.timestamp).toBeLessThanOrEqual(Date.now());
  });

  it('getQuote returns null when result.list is empty (symbol not found)', async () => {
    mockFetch({
      retCode: 0,
      retMsg: 'OK',
      result: { category: 'spot', list: [] },
      time: 1730000000000,
    });

    const provider = new BybitProvider();
    expect(await provider.getQuote(bybitPair)).toBeNull();
  });

  it('getQuote throws on Bybit retCode != 0', async () => {
    mockFetch({
      retCode: 10001,
      retMsg: 'params error',
      result: { category: 'spot', list: [] },
      time: 1730000000000,
    });

    const provider = new BybitProvider();
    await expect(provider.getQuote(bybitPair)).rejects.toThrow(/retCode=10001/);
  });

  it('getQuote returns null on zero / non-finite prices', async () => {
    mockFetch({
      retCode: 0,
      retMsg: 'OK',
      result: {
        category: 'spot',
        list: [{ symbol: 'CCUSDT', bid1Price: '0', ask1Price: '0', lastPrice: '0' }],
      },
      time: 1730000000000,
    });

    const provider = new BybitProvider();
    expect(await provider.getQuote(bybitPair)).toBeNull();
  });

  it('respects custom baseUrl (testnet)', async () => {
    const fetchMock = vi.fn(async () =>
      Promise.resolve({
        ok: true,
        status: 200,
        text: async () =>
          JSON.stringify({
            retCode: 0,
            retMsg: 'OK',
            result: {
              category: 'spot',
              list: [
                { symbol: 'CCUSDT', bid1Price: '0.15', ask1Price: '0.16', lastPrice: '0.155' },
              ],
            },
            time: 1730000000000,
          }),
      } as Response),
    );
    vi.stubGlobal('fetch', fetchMock);

    const provider = new BybitProvider({ baseUrl: 'https://api-testnet.bybit.com' });
    await provider.getQuote(bybitPair);

    expect(fetchMock).toHaveBeenCalledWith(
      expect.stringContaining('https://api-testnet.bybit.com'),
      expect.any(Object),
    );
  });
});
