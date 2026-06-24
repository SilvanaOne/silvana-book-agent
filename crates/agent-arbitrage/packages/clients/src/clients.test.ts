import { afterEach, describe, expect, it, vi } from 'vitest';
import { createPair, createSpreadOpportunity, createToken, type DexQuote } from '@arbitrage-agent/shared';
import { authHeaders, canonicalString, signRequest, SIGNATURE_HEADER, TIMESTAMP_HEADER } from './auth.js';
import { ExecutorClient, toOpportunityDTO } from './executor-client.js';
import { executorClientFromEnv, signerClientFromEnv } from './env.js';

describe('auth', () => {
  it('canonicalString joins fields with newlines and upper-cases method', () => {
    expect(canonicalString('123', 'post', '/x', '{}')).toBe('123\nPOST\n/x\n{}');
  });

  it('signRequest is deterministic for the same inputs', () => {
    const a = signRequest('secret', '123', 'POST', '/execute-dual-trade', '{"a":1}');
    const b = signRequest('secret', '123', 'POST', '/execute-dual-trade', '{"a":1}');
    expect(a).toBe(b);
    expect(a).toMatch(/^[0-9a-f]{64}$/);
  });

  it('signRequest changes when any field changes', () => {
    const base = signRequest('secret', '123', 'POST', '/x', '{}');
    expect(signRequest('secret2', '123', 'POST', '/x', '{}')).not.toBe(base);
    expect(signRequest('secret', '124', 'POST', '/x', '{}')).not.toBe(base);
    expect(signRequest('secret', '123', 'GET', '/x', '{}')).not.toBe(base);
    expect(signRequest('secret', '123', 'POST', '/y', '{}')).not.toBe(base);
    expect(signRequest('secret', '123', 'POST', '/x', '{"a":1}')).not.toBe(base);
  });

  it('authHeaders uses injected clock and embeds the signature', () => {
    const headers = authHeaders('secret', 'GET', '/health', '', () => 1700000000000);
    expect(headers[TIMESTAMP_HEADER]).toBe('1700000000000');
    expect(headers[SIGNATURE_HEADER]).toBe(`sha256=${signRequest('secret', '1700000000000', 'GET', '/health', '')}`);
  });
});

describe('SignerClient GET with query params', () => {
  afterEach(() => vi.unstubAllGlobals());

  it('signs pathname only while sending the full URL with query', async () => {
    const calls: Array<{ url: string; init: RequestInit }> = [];
    vi.stubGlobal('fetch', async (url: string, init: RequestInit) => {
      calls.push({ url, init });
      return {
        ok: true,
        status: 200,
        text: async () => JSON.stringify({ updatedAt: 1, totalUsd: 0, sources: [], rows: [] }),
      } as unknown as Response;
    });

    const { SignerClient } = await import('./signer-client.js');
    const client = new SignerClient({
      baseUrl: 'http://localhost:4003',
      token: 'secret',
      now: () => 1700000000000,
    });
    await client.getBalances('47436409-c5d2-465a-adc8-24357190d846', true);

    expect(calls).toHaveLength(1);
    const call = calls[0]!;
    expect(call.url).toContain('/balances?tenantUserId=');
    const headers = call.init.headers as Record<string, string>;
    expect(headers[SIGNATURE_HEADER]).toBe(
      `sha256=${signRequest('secret', '1700000000000', 'GET', '/balances', '')}`,
    );
  });
});

describe('toOpportunityDTO', () => {
  it('flattens a shared SpreadOpportunity into the wire DTO', () => {
    const cc = createToken('CC', 'Canton Coin', 10, 'cex');
    const usdt = createToken('USDT', 'Tether', 6, 'cex');
    const pair = createPair(cc, usdt, 'bybit');
    const buyFrom: DexQuote = { venueId: 'bybit', buyPrice: 0.15, sellPrice: 0.151, timestamp: 1 };
    const sellTo: DexQuote = { venueId: 'kucoin', buyPrice: 0.158, sellPrice: 0.159, timestamp: 1 };
    const opp = createSpreadOpportunity(pair, buyFrom, sellTo, 1000);

    const dto = toOpportunityDTO(opp);
    expect(dto.baseSymbol).toBe('CC');
    expect(dto.quoteSymbol).toBe('USDT');
    expect(dto.buyVenue).toBe('bybit');
    expect(dto.sellVenue).toBe('kucoin');
    expect(dto.buyPrice).toBe(0.15);
    expect(dto.sellPrice).toBe(0.159);
    expect(typeof dto.detectedAt).toBe('number');
  });
});

describe('ExecutorClient request shaping', () => {
  afterEach(() => vi.unstubAllGlobals());

  it('POSTs to the right URL with signed headers and JSON body', async () => {
    const calls: Array<{ url: string; init: RequestInit }> = [];
    vi.stubGlobal('fetch', async (url: string, init: RequestInit) => {
      calls.push({ url, init });
      return {
        ok: true,
        status: 202,
        text: async () => JSON.stringify({ tradeId: 't-1', status: 'queued' }),
      } as unknown as Response;
    });

    const client = new ExecutorClient({
      baseUrl: 'http://localhost:4001',
      token: 'secret',
      now: () => 1700000000000,
    });
    const res = await client.executeDualTrade({
      opportunity: {
        baseSymbol: 'CC',
        quoteSymbol: 'USDT',
        buyVenue: 'bybit',
        sellVenue: 'kucoin',
        buyPrice: 0.15,
        sellPrice: 0.159,
        spreadPct: 6,
        estimatedProfitUsd: 60,
        detectedAt: 1700000000000,
      },
      maxSlippageBps: 20,
      safetyBufferBps: 5,
      dryRun: true,
      idempotencyKey: 'idem-1',
    });

    expect(res).toEqual({ tradeId: 't-1', status: 'queued' });
    expect(calls).toHaveLength(1);
    const call = calls[0]!;
    expect(call.url).toBe('http://localhost:4001/execute-dual-trade');
    expect(call.init.method).toBe('POST');
    const headers = call.init.headers as Record<string, string>;
    expect(headers[TIMESTAMP_HEADER]).toBe('1700000000000');
    expect(headers[SIGNATURE_HEADER]).toMatch(/^sha256=[0-9a-f]{64}$/);
    expect(headers['Content-Type']).toBe('application/json');
  });
});

describe('fromEnv factories', () => {
  it('return null when the engine URL is absent', () => {
    expect(executorClientFromEnv({})).toBeNull();
    expect(signerClientFromEnv({})).toBeNull();
  });

  it('build a client when URL is present and strip trailing slash', () => {
    const client = executorClientFromEnv({ EXECUTOR_API_URL: 'http://localhost:4001/', EXECUTOR_API_TOKEN: 's' });
    expect(client).toBeInstanceOf(ExecutorClient);
  });
});
