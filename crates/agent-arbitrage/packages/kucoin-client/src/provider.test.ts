import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { createToken, createPair } from '@arbitrage-agent/shared';
import { KucoinProvider } from './provider.js';
import { pairToKucoinSymbol } from './symbol.js';

const CC = createToken('CC', 'Canton Coin', 6, 'canton');
const USDT = createToken('USDT', 'Tether USD', 6, 'cex');
const kucoinPair = createPair(CC, USDT, 'kucoin');
const bybitPair = createPair(CC, USDT, 'bybit');

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

describe('pairToKucoinSymbol', () => {
  it('hyphenates base-quote uppercase', () => {
    expect(pairToKucoinSymbol(kucoinPair)).toBe('CC-USDT');
  });
});

describe('KucoinProvider', () => {
  beforeEach(() => {
    vi.unstubAllGlobals();
  });
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('supportsPair returns true for venueId=kucoin, false otherwise', () => {
    const provider = new KucoinProvider();
    expect(provider.supportsPair(kucoinPair)).toBe(true);
    expect(provider.supportsPair(bybitPair)).toBe(false);
  });

  it('getQuote returns null when pair is not supported', async () => {
    const provider = new KucoinProvider();
    expect(await provider.getQuote(bybitPair)).toBeNull();
  });

  it('getQuote maps bestAsk → buyPrice and bestBid → sellPrice', async () => {
    mockFetch({
      code: '200000',
      data: {
        sequence: '12345',
        price: '0.155',
        size: '100',
        bestBid: '0.149',
        bestBidSize: '500',
        bestAsk: '0.162',
        bestAskSize: '300',
        time: 1730000000000,
      },
    });

    const provider = new KucoinProvider();
    const quote = await provider.getQuote(kucoinPair);

    expect(quote).not.toBeNull();
    expect(quote!.venueId).toBe('kucoin');
    expect(quote!.buyPrice).toBe(0.162);
    expect(quote!.sellPrice).toBe(0.149);
    // timestamp is our observation time (Date.now() at parse time), not KuCoin's ticker.time
    expect(quote!.timestamp).toBeGreaterThan(Date.now() - 1000);
    expect(quote!.timestamp).toBeLessThanOrEqual(Date.now());
  });

  it('getQuote returns null when data is null (symbol not found)', async () => {
    mockFetch({ code: '200000', data: null });

    const provider = new KucoinProvider();
    expect(await provider.getQuote(kucoinPair)).toBeNull();
  });

  it('getQuote throws on KuCoin non-200000 code', async () => {
    mockFetch({ code: '400100', data: null, msg: 'invalid symbol' });

    const provider = new KucoinProvider();
    await expect(provider.getQuote(kucoinPair)).rejects.toThrow(/code=400100/);
  });

  it('getQuote returns null on zero / non-finite prices', async () => {
    mockFetch({
      code: '200000',
      data: {
        sequence: '12345',
        price: '0',
        size: '0',
        bestBid: '0',
        bestBidSize: '0',
        bestAsk: '0',
        bestAskSize: '0',
        time: 1730000000000,
      },
    });

    const provider = new KucoinProvider();
    expect(await provider.getQuote(kucoinPair)).toBeNull();
  });

  it('respects custom baseUrl', async () => {
    const fetchMock = vi.fn(async () =>
      Promise.resolve({
        ok: true,
        status: 200,
        text: async () =>
          JSON.stringify({
            code: '200000',
            data: {
              sequence: '1',
              price: '0.155',
              size: '100',
              bestBid: '0.15',
              bestBidSize: '500',
              bestAsk: '0.16',
              bestAskSize: '300',
              time: 1730000000000,
            },
          }),
      } as Response),
    );
    vi.stubGlobal('fetch', fetchMock);

    const provider = new KucoinProvider({ baseUrl: 'https://openapi-sandbox.kucoin.com' });
    await provider.getQuote(kucoinPair);

    expect(fetchMock).toHaveBeenCalledWith(
      expect.stringContaining('https://openapi-sandbox.kucoin.com'),
      expect.any(Object),
    );
  });
});
