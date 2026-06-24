/**
 * Typed client for open-core REST API (apps/api).
 *
 * Sprint 2.3: HMAC bearer tokens (Authorization header for fetch, ?token=
 * for SSE since EventSource cannot set headers).
 * Sprint 2.4: config mutations + kill switch.
 */

import { getStoredToken } from './auth';

const API_BASE = process.env.NEXT_PUBLIC_API_URL ?? 'http://localhost:3000';

function authHeaders(): Record<string, string> {
  const t = getStoredToken();
  return t ? { authorization: `Bearer ${t}` } : {};
}

async function jsonFetch<T>(
  path: string,
  init?: RequestInit & { signal?: AbortSignal },
): Promise<T> {
  const res = await fetch(`${API_BASE}${path}`, {
    ...init,
    headers: {
      ...(init?.body ? { 'content-type': 'application/json' } : {}),
      ...authHeaders(),
      ...(init?.headers ?? {}),
    },
  });
  if (!res.ok) {
    const body = (await res.json().catch(() => ({}))) as { error?: string };
    throw new Error(body.error ?? `HTTP ${res.status}`);
  }
  return (await res.json()) as T;
}

export interface SpreadDto {
  readonly id: string;
  readonly ts: string;
  readonly basePairKey: string;
  readonly buyVenueId: string;
  readonly sellVenueId: string;
  readonly buyPrice: string;
  readonly sellPrice: string;
  readonly spreadBps: number;
  readonly estProfitUsd: string;
  readonly acted: boolean;
}

export interface SpreadsResponse {
  readonly spreads: readonly SpreadDto[];
  readonly count: number;
  readonly limit: number;
}

export async function fetchSpreads(limit = 100, signal?: AbortSignal): Promise<SpreadsResponse> {
  return jsonFetch<SpreadsResponse>(`/api/spreads?limit=${limit}`, { signal });
}

export interface RuntimeConfig {
  readonly tradeConfig: {
    readonly targetSpreadPercent: number;
    readonly maxSpreadPercent: number;
    readonly tradeSizeUsd: number;
  };
  readonly riskConfig: {
    readonly maxSpreadBps: number;
    readonly maxQuoteAgeMs: number;
    readonly dailyLossLimitUsd: number;
    readonly maxConsecutiveLosses: number;
  };
  readonly runtime: {
    readonly strategyMode: string;
    readonly silvanaHostMode: string;
    readonly scanIntervalMs: number;
    readonly scannerPersist: boolean;
    readonly killSwitch: boolean;
  };
}

export async function fetchConfig(signal?: AbortSignal): Promise<RuntimeConfig> {
  return jsonFetch<RuntimeConfig>(`/api/config`, { signal });
}

export async function updateTradeConfig(
  patch: Partial<RuntimeConfig['tradeConfig']>,
): Promise<RuntimeConfig['tradeConfig']> {
  return jsonFetch(`/api/config/trade`, { method: 'PUT', body: JSON.stringify(patch) });
}

export async function updateRiskConfig(
  patch: Partial<RuntimeConfig['riskConfig']>,
): Promise<RuntimeConfig['riskConfig']> {
  return jsonFetch(`/api/config/risk`, { method: 'PUT', body: JSON.stringify(patch) });
}

export async function setKillSwitch(enabled: boolean): Promise<{ killSwitch: boolean }> {
  return jsonFetch(`/api/config/kill-switch`, {
    method: 'POST',
    body: JSON.stringify({ enabled }),
  });
}

export interface VenueHealth {
  readonly venueId: string;
  readonly displayName: string;
  readonly enabled: boolean;
  readonly status: 'healthy' | 'stale' | 'down';
  readonly lastSuccessAt: string | null;
  readonly lastErrorAt: string | null;
  readonly lastError: string | null;
  readonly successStreak: number;
  readonly errorStreak: number;
}

export async function fetchVenueHealth(signal?: AbortSignal): Promise<{ venues: VenueHealth[] }> {
  return jsonFetch(`/api/venues/health`, { signal });
}

export interface Stats {
  readonly windowMs: number;
  readonly spreads: {
    readonly total: number;
    readonly lastHour: number;
    readonly avgBps: number;
    readonly maxBps: number;
    readonly byRoute: ReadonlyArray<{ route: string; count: number }>;
  };
  readonly pnl: {
    readonly actedCount: number;
    readonly estimatedUsd: string;
    readonly realizedUsd: string;
  };
}

export async function fetchStats(signal?: AbortSignal): Promise<Stats> {
  return jsonFetch(`/api/stats`, { signal });
}

export interface Profitability {
  readonly totals: {
    readonly count: number;
    readonly estProfitUsd: string;
    readonly avgBps: number;
    readonly maxBps: number;
  };
  readonly byVenue: ReadonlyArray<{
    readonly venueId: string;
    readonly count: number;
    readonly buyCount: number;
    readonly sellCount: number;
    readonly estProfitUsd: string;
  }>;
  readonly bySize: ReadonlyArray<{
    readonly label: string;
    readonly min: number;
    readonly max: number | null;
    readonly count: number;
  }>;
}

export async function fetchProfitability(signal?: AbortSignal): Promise<Profitability> {
  return jsonFetch(`/api/stats/profitability`, { signal });
}

export interface AuditEntry {
  readonly id: string;
  readonly ts: string;
  readonly actor: string;
  readonly action: string;
  readonly payload: unknown;
}

export async function fetchAuditLog(limit = 20, signal?: AbortSignal): Promise<{ entries: AuditEntry[] }> {
  return jsonFetch(`/api/activity/audit?limit=${limit}`, { signal });
}

export interface TradeEntry {
  readonly id: string;
  readonly ts: string;
  readonly executorTradeId: string | null;
  readonly ticker: string;
  readonly buyVenueId: string;
  readonly sellVenueId: string;
  readonly status: string; // SUCCESS | PARTIAL | FAILED | SKIPPED
  readonly partialExecution: boolean;
  readonly profitUsdtDiff: string | null;
  readonly spreadBps: number | null;
  readonly legBuyStatus: string | null;
  readonly legSellStatus: string | null;
  readonly failReason: string | null;
}

export async function fetchTrades(limit = 20, signal?: AbortSignal): Promise<{ trades: TradeEntry[] }> {
  return jsonFetch(`/api/activity/trades?limit=${limit}`, { signal });
}

export interface Holding {
  readonly venueId: string;
  readonly asset: string;
  readonly free: string;
  readonly locked: string;
  readonly ts: string;
}

export async function fetchInventory(signal?: AbortSignal): Promise<{ holdings: Holding[] }> {
  return jsonFetch(`/api/activity/inventory`, { signal });
}

export interface BalanceSource {
  readonly id: string;
  readonly label: string;
  readonly kind: 'cex' | 'canton';
  readonly totalUsd: number;
  readonly error?: string;
}

export interface BalanceRow {
  readonly sourceId: string;
  readonly sourceLabel: string;
  readonly kind: 'cex' | 'canton';
  readonly venueOrChain: string;
  readonly asset: string;
  readonly free: number;
  readonly locked: number;
  readonly usd: number | null;
  readonly address?: string;
}

export interface BalanceSnapshot {
  readonly updatedAt: number;
  readonly totalUsd: number;
  readonly sources: readonly BalanceSource[];
  readonly rows: readonly BalanceRow[];
}

/** Cached balances — never waits on CEX; background refresh on API (~30s). */
export async function fetchBalances(signal?: AbortSignal): Promise<BalanceSnapshot> {
  return jsonFetch<BalanceSnapshot>(`/api/balances`, { signal });
}

export interface SpreadFeedHandlers {
  readonly onSpread: (spread: SpreadDto) => void;
  readonly onOpen?: () => void;
  readonly onError?: (err: Event) => void;
}

/**
 * Subscribe to live spread events via SSE. Returns an unsubscribe function.
 * EventSource cannot set headers so we pass the bearer token as `?token=`.
 */
export function subscribeToSpreads(handlers: SpreadFeedHandlers): () => void {
  const token = getStoredToken();
  const url = token
    ? `${API_BASE}/api/events/stream?token=${encodeURIComponent(token)}`
    : `${API_BASE}/api/events/stream`;
  const es = new EventSource(url);

  es.addEventListener('open', () => handlers.onOpen?.());
  es.addEventListener('error', (e) => handlers.onError?.(e));
  es.addEventListener('spread', (e) => {
    try {
      const data = JSON.parse((e as MessageEvent).data) as SpreadDto;
      handlers.onSpread(data);
    } catch {
      // malformed payload — server should never send these
    }
  });

  return () => es.close();
}

// ── Signer / Keys Manager (U1) ───────────────────────────────────────────
// Proxied through apps/api → proprietary vault. The browser fetches the vault
// public key, encrypts each secret locally (see lib/crypto.rsaOaepEncrypt), and
// posts only ciphertext. Plaintext never leaves the browser unencrypted.

export interface SignerPubkey {
  readonly pem: string;
  readonly keyId: string;
}

export interface SecretSlot {
  readonly id: string;
  readonly kind: 'cex' | 'wallet';
  readonly filled: boolean;
  readonly venue?: string;
  readonly label?: string;
  readonly rsaKeyId?: string;
}

export interface SecretsStatus {
  readonly slots: readonly SecretSlot[];
}

export interface SaveCexBody {
  readonly venue: string;
  readonly keyId: string;
  readonly apiKeyCiphertext: string;
  readonly secretCiphertext: string;
  readonly passphraseCiphertext?: string;
}

export interface SaveWalletBody {
  readonly label: string;
  readonly chain: string;
  readonly keyId: string;
  readonly privKeyCiphertext?: string;
  readonly seedCiphertext?: string;
}

/** POST with no expected response body (204). Throws on non-2xx. */
async function postNoContent(path: string, body: unknown): Promise<void> {
  const res = await fetch(`${API_BASE}${path}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', ...authHeaders() },
    body: JSON.stringify(body),
  });
  if (!res.ok) {
    const b = (await res.json().catch(() => ({}))) as { error?: string; detail?: string };
    throw new Error(b.detail ?? b.error ?? `HTTP ${res.status}`);
  }
}

export async function fetchSignerPubkey(signal?: AbortSignal): Promise<SignerPubkey> {
  return jsonFetch<SignerPubkey>('/api/signer/pubkey', { signal });
}

export async function fetchSecretsStatus(signal?: AbortSignal): Promise<SecretsStatus> {
  return jsonFetch<SecretsStatus>('/api/signer/status', { signal });
}

export async function saveCexSecret(body: SaveCexBody): Promise<void> {
  return postNoContent('/api/signer/secrets/cex', body);
}

export async function saveWalletSecret(body: SaveWalletBody): Promise<void> {
  return postNoContent('/api/signer/secrets/wallet', body);
}
