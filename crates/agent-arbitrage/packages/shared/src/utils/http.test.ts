import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { fetchJson, HttpError } from './http.js';

function mockFetchSequence(
  responses: Array<{ ok?: boolean; status?: number; body?: string } | Error>,
): ReturnType<typeof vi.fn> {
  let i = 0;
  const fn = vi.fn(async () => {
    if (i >= responses.length) throw new Error(`fetch called beyond mocked count (${i + 1})`);
    const r = responses[i++]!;
    if (r instanceof Error) throw r;
    const status = r.status ?? 200;
    const ok = r.ok ?? (status >= 200 && status < 300);
    return {
      ok,
      status,
      text: async () => r.body ?? '{}',
    } as Response;
  });
  vi.stubGlobal('fetch', fn);
  return fn;
}

describe('fetchJson — happy path', () => {
  beforeEach(() => {
    vi.unstubAllGlobals();
  });
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('returns parsed JSON on 200', async () => {
    mockFetchSequence([{ body: '{"hello":"world"}' }]);
    const r = await fetchJson<{ hello: string }>('https://x.com', { maxRetries: 0 });
    expect(r.hello).toBe('world');
  });
});

describe('fetchJson — retry semantics', () => {
  beforeEach(() => {
    vi.unstubAllGlobals();
  });
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('retries on network error then succeeds', async () => {
    const fn = mockFetchSequence([
      new TypeError('fetch failed'),
      { body: '{"ok":true}' },
    ]);
    const onRetry = vi.fn();
    const r = await fetchJson<{ ok: boolean }>('https://x.com', {
      maxRetries: 3,
      retryBaseMs: 1,
      onRetry,
    });
    expect(r.ok).toBe(true);
    expect(fn).toHaveBeenCalledTimes(2);
    expect(onRetry).toHaveBeenCalledTimes(1);
    expect(onRetry).toHaveBeenCalledWith(1, expect.any(TypeError));
  });

  it('retries on 503 then succeeds', async () => {
    const fn = mockFetchSequence([
      { status: 503, body: 'try again' },
      { body: '{"ok":true}' },
    ]);
    const r = await fetchJson<{ ok: boolean }>('https://x.com', {
      maxRetries: 3,
      retryBaseMs: 1,
    });
    expect(r.ok).toBe(true);
    expect(fn).toHaveBeenCalledTimes(2);
  });

  it('does NOT retry on 4xx — throws immediately', async () => {
    const fn = mockFetchSequence([{ status: 400, body: 'bad input' }]);
    await expect(
      fetchJson('https://x.com', { maxRetries: 3, retryBaseMs: 1 }),
    ).rejects.toThrow(HttpError);
    expect(fn).toHaveBeenCalledTimes(1);
  });

  it('throws after exhausting retries', async () => {
    const fn = mockFetchSequence([
      new Error('net1'),
      new Error('net2'),
      new Error('net3'),
    ]);
    await expect(
      fetchJson('https://x.com', { maxRetries: 2, retryBaseMs: 1 }),
    ).rejects.toThrow('net3');
    expect(fn).toHaveBeenCalledTimes(3); // 1 attempt + 2 retries
  });

  it('maxRetries=0 disables retry', async () => {
    const fn = mockFetchSequence([new Error('net1'), new Error('net2')]);
    await expect(
      fetchJson('https://x.com', { maxRetries: 0 }),
    ).rejects.toThrow('net1');
    expect(fn).toHaveBeenCalledTimes(1);
  });

  it('throws HttpError on invalid JSON (no retry — body was 2xx)', async () => {
    mockFetchSequence([{ body: 'not json' }]);
    await expect(fetchJson('https://x.com', { maxRetries: 3, retryBaseMs: 1 })).rejects.toThrow(
      /Invalid JSON/,
    );
  });
});
