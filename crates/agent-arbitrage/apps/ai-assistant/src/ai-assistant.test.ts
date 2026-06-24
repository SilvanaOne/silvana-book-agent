import { createHmac } from 'node:crypto';
import type { AddressInfo } from 'node:net';
import type { Server } from 'node:http';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { createApp } from './server.js';
import type { Config } from './config.js';

const CFG: Config = { port: 0, apiToken: 'test-ai', maxSkewSec: 300, enabled: true, provider: 'heuristic' };

function sign(method: string, path: string, body: string): Record<string, string> {
  const ts = String(Date.now());
  const mac = createHmac('sha256', CFG.apiToken).update(`${ts}\n${method.toUpperCase()}\n${path}\n${body}`).digest('hex');
  return { 'X-Arb-Timestamp': ts, 'X-Arb-Signature': `sha256=${mac}`, 'Content-Type': 'application/json' };
}

function listen(app: ReturnType<typeof createApp>): Promise<{ server: Server; base: string }> {
  return new Promise((resolve) => {
    const server = app.listen(0, () => resolve({ server, base: `http://127.0.0.1:${(server.address() as AddressInfo).port}` }));
  });
}

describe('ai-assistant service', () => {
  let server: Server;
  let base: string;
  beforeAll(async () => {
    ({ server, base } = await listen(createApp(CFG)));
  });
  afterAll(() => new Promise<void>((r) => server.close(() => r())));

  it('health is open and reports heuristic provider', async () => {
    const h = await (await fetch(`${base}/health`)).json();
    expect(h.status).toBe('ok');
    expect(h.llm).toBe('heuristic');
  });

  it('rejects unsigned advisory calls', async () => {
    expect((await fetch(`${base}/ai/resolve-symbol`, { method: 'POST' })).status).toBe(401);
  });

  it('trade-postmortem returns a suggestion', async () => {
    const body = JSON.stringify({ trade: { ticker: 'CC/USDT', buyVenueId: 'bybit', sellVenueId: 'kucoin', status: 'PARTIAL', failReason: 'sell rejected' } });
    const res = await fetch(`${base}/ai/trade-postmortem`, { method: 'POST', headers: sign('POST', '/ai/trade-postmortem', body), body });
    expect(res.status).toBe(200);
    const j = await res.json();
    expect(j.whatHappened).toMatch(/Buy leg filled/);
    expect(j.suggestedActions.length).toBeGreaterThan(0);
  });

  it('config-suggest → approval → confirm round-trip', async () => {
    const body = JSON.stringify({ naturalLanguage: 'set min spread to 30 bps', currentConfig: {} });
    const sug = await (await fetch(`${base}/ai/config-suggest`, { method: 'POST', headers: sign('POST', '/ai/config-suggest', body), body })).json();
    expect(sug.confirmationRequired).toBe(true);
    expect(sug.approvalId).toBeTruthy();

    const cpath = `/ai/approvals/${sug.approvalId}/confirm`;
    const conf = await fetch(`${base}${cpath}`, { method: 'POST', headers: sign('POST', cpath, '') });
    expect(conf.status).toBe(200);
    expect((await conf.json()).approved).toBe(true);
  });
});

describe('ai-assistant kill-switch', () => {
  it('returns 503 on /ai when disabled, health still ok', async () => {
    const { server, base } = await listen(createApp({ ...CFG, enabled: false }));
    try {
      expect((await fetch(`${base}/health`)).status).toBe(200);
      const body = JSON.stringify({ query: 'CC' });
      const res = await fetch(`${base}/ai/resolve-symbol`, { method: 'POST', headers: sign('POST', '/ai/resolve-symbol', body), body });
      expect(res.status).toBe(503);
    } finally {
      await new Promise<void>((r) => server.close(() => r()));
    }
  });
});
