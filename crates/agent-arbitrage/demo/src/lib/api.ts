/**
 * DEMO client — fully offline.
 *
 * This file replaces the real open-core REST client. Every function here
 * returns synthetic fixtures (from lib/demo.ts + local fixtures below) instead
 * of hitting a backend. There is NO fetch / EventSource / network access at
 * runtime — this is a static, self-contained showcase of the operator UI.
 *
 * The public surface (types + function signatures) is kept identical to the
 * real client so the components/pages compile and behave unchanged.
 */

import { demoSpreads, demoProfitability } from './demo';

// ── shared types (identical to the real client) ─────────────────────────────

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

export interface AuditEntry {
  readonly id: string;
  readonly ts: string;
  readonly actor: string;
  readonly action: string;
  readonly payload: unknown;
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

export interface Holding {
  readonly venueId: string;
  readonly asset: string;
  readonly free: string;
  readonly locked: string;
  readonly ts: string;
}

export interface SpreadFeedHandlers {
  readonly onSpread: (spread: SpreadDto) => void;
  readonly onOpen?: () => void;
  readonly onError?: (err: Event) => void;
}

// ── in-memory mutable runtime config (edits "stick" for the session) ─────────

let RUNTIME_CONFIG: RuntimeConfig = {
  tradeConfig: { targetSpreadPercent: 0.8, maxSpreadPercent: 5, tradeSizeUsd: 250 },
  riskConfig: {
    maxSpreadBps: 500,
    maxQuoteAgeMs: 2000,
    dailyLossLimitUsd: 500,
    maxConsecutiveLosses: 3,
  },
  runtime: {
    strategyMode: 'scan-only',
    silvanaHostMode: 'mock',
    scanIntervalMs: 1500,
    scannerPersist: true,
    killSwitch: false,
  },
};

/** Small helper so every "fetch" resolves on a microtask like the real client. */
function resolve<T>(value: T): Promise<T> {
  return Promise.resolve(value);
}

// ── spreads ─────────────────────────────────────────────────────────────────

export async function fetchSpreads(limit = 100, _signal?: AbortSignal): Promise<SpreadsResponse> {
  const all = demoSpreads();
  const sorted = [...all].sort((a, b) => new Date(b.ts).getTime() - new Date(a.ts).getTime());
  const spreads = sorted.slice(0, limit);
  return resolve({ spreads, count: all.length, limit });
}

// ── config (read + mutate, all in-memory) ────────────────────────────────────

export async function fetchConfig(_signal?: AbortSignal): Promise<RuntimeConfig> {
  return resolve(RUNTIME_CONFIG);
}

export async function updateTradeConfig(
  patch: Partial<RuntimeConfig['tradeConfig']>,
): Promise<RuntimeConfig['tradeConfig']> {
  RUNTIME_CONFIG = { ...RUNTIME_CONFIG, tradeConfig: { ...RUNTIME_CONFIG.tradeConfig, ...patch } };
  return resolve(RUNTIME_CONFIG.tradeConfig);
}

export async function updateRiskConfig(
  patch: Partial<RuntimeConfig['riskConfig']>,
): Promise<RuntimeConfig['riskConfig']> {
  RUNTIME_CONFIG = { ...RUNTIME_CONFIG, riskConfig: { ...RUNTIME_CONFIG.riskConfig, ...patch } };
  return resolve(RUNTIME_CONFIG.riskConfig);
}

export async function setKillSwitch(enabled: boolean): Promise<{ killSwitch: boolean }> {
  RUNTIME_CONFIG = {
    ...RUNTIME_CONFIG,
    runtime: { ...RUNTIME_CONFIG.runtime, killSwitch: enabled },
  };
  return resolve({ killSwitch: enabled });
}

// ── venue health ─────────────────────────────────────────────────────────────

const NOW_ISO = () => new Date().toISOString();

const VENUE_HEALTH: readonly VenueHealth[] = [
  { venueId: 'silvana', displayName: 'Silvana Book', enabled: true, status: 'healthy', lastSuccessAt: null, lastErrorAt: null, lastError: null, successStreak: 512, errorStreak: 0 },
  { venueId: 'temple', displayName: 'Temple', enabled: true, status: 'healthy', lastSuccessAt: null, lastErrorAt: null, lastError: null, successStreak: 318, errorStreak: 0 },
  { venueId: 'cantex', displayName: 'Cantex', enabled: true, status: 'healthy', lastSuccessAt: null, lastErrorAt: null, lastError: null, successStreak: 274, errorStreak: 0 },
  { venueId: 'oneswap', displayName: 'OneSwap', enabled: true, status: 'healthy', lastSuccessAt: null, lastErrorAt: null, lastError: null, successStreak: 201, errorStreak: 0 },
  { venueId: 'bybit', displayName: 'Bybit', enabled: true, status: 'healthy', lastSuccessAt: null, lastErrorAt: null, lastError: null, successStreak: 1893, errorStreak: 0 },
  { venueId: 'kucoin', displayName: 'KuCoin', enabled: true, status: 'healthy', lastSuccessAt: null, lastErrorAt: null, lastError: null, successStreak: 1420, errorStreak: 0 },
];

export async function fetchVenueHealth(_signal?: AbortSignal): Promise<{ venues: VenueHealth[] }> {
  const ts = NOW_ISO();
  return resolve({ venues: VENUE_HEALTH.map((v) => ({ ...v, lastSuccessAt: ts })) });
}

// ── stats + profitability ─────────────────────────────────────────────────────

export async function fetchStats(_signal?: AbortSignal): Promise<Stats> {
  return resolve({
    windowMs: 3_600_000,
    spreads: {
      total: 42,
      lastHour: 18,
      avgBps: 185,
      maxBps: 410,
      byRoute: [
        { route: 'silvana → cantex', count: 12 },
        { route: 'silvana → temple', count: 9 },
        { route: 'oneswap → temple', count: 7 },
        { route: 'temple → silvana', count: 6 },
      ],
    },
    pnl: { actedCount: 8, estimatedUsd: '3.18', realizedUsd: '0' },
  });
}

export async function fetchProfitability(_signal?: AbortSignal): Promise<Profitability> {
  return resolve(demoProfitability());
}

// ── activity feeds (trades / inventory / audit) ──────────────────────────────

export async function fetchTrades(_limit = 20, _signal?: AbortSignal): Promise<{ trades: TradeEntry[] }> {
  const base = Date.now();
  const mk = (
    i: number,
    ticker: string,
    buy: string,
    sell: string,
    status: string,
    pnl: string | null,
    bps: number | null,
  ): TradeEntry => ({
    id: `trade-${i}`,
    ts: new Date(base - i * 90_000).toISOString(),
    executorTradeId: status === 'SKIPPED' ? null : `ex-${1000 + i}`,
    ticker,
    buyVenueId: buy,
    sellVenueId: sell,
    status,
    partialExecution: status === 'PARTIAL',
    profitUsdtDiff: pnl,
    spreadBps: bps,
    legBuyStatus: status === 'SKIPPED' ? null : 'FILLED',
    legSellStatus: status === 'PARTIAL' ? 'PARTIAL' : status === 'SKIPPED' ? null : 'FILLED',
    failReason: status === 'FAILED' ? 'quote stale' : null,
  });
  return resolve({
    trades: [
      mk(1, 'CC/USDC', 'silvana', 'cantex', 'SUCCESS', '0.42', 210),
      mk(2, 'CBTC/USDC', 'silvana', 'temple', 'SUCCESS', '0.88', 358),
      mk(3, 'CC/USDCx', 'oneswap', 'cantex', 'PARTIAL', '0.11', 178),
      mk(4, 'CETH/USDC', 'silvana', 'temple', 'SUCCESS', '0.53', 228),
      mk(5, 'CC/CBTC', 'oneswap', 'temple', 'SKIPPED', null, 132),
    ],
  });
}

export async function fetchInventory(_signal?: AbortSignal): Promise<{ holdings: Holding[] }> {
  const ts = NOW_ISO();
  const mk = (venueId: string, asset: string, free: string, locked: string): Holding => ({
    venueId,
    asset,
    free,
    locked,
    ts,
  });
  return resolve({
    holdings: [
      mk('silvana', 'CC', '18420.5', '250'),
      mk('silvana', 'USDC', '9120.44', '0'),
      mk('temple', 'CBTC', '0.184', '0'),
      mk('cantex', 'USDC', '4300.1', '120'),
      mk('bybit', 'USDT', '5000', '0'),
      mk('kucoin', 'USDT', '2500', '0'),
    ],
  });
}

export async function fetchAuditLog(_limit = 20, _signal?: AbortSignal): Promise<{ entries: AuditEntry[] }> {
  const base = Date.now();
  return resolve({
    entries: [
      { id: 'a-1', ts: new Date(base - 120_000).toISOString(), actor: 'operator', action: 'config.trade.update', payload: { tradeSizeUsd: 250 } },
      { id: 'a-2', ts: new Date(base - 600_000).toISOString(), actor: 'operator', action: 'killswitch.release', payload: {} },
      { id: 'a-3', ts: new Date(base - 1_800_000).toISOString(), actor: 'operator', action: 'config.risk.update', payload: { dailyLossLimitUsd: 500 } },
    ],
  });
}

// ── live spread feed (local interval — no EventSource) ───────────────────────

const LIVE_PAIRS: ReadonlyArray<{
  readonly pairKey: string;
  readonly buyVenue: string;
  readonly sellVenue: string;
  readonly buyPrice: string;
  readonly baseBps: number;
}> = [
  { pairKey: 'CC/USDC', buyVenue: 'silvana', sellVenue: 'cantex', buyPrice: '0.0212', baseBps: 190 },
  { pairKey: 'CC/USDCx', buyVenue: 'silvana', sellVenue: 'oneswap', buyPrice: '0.0205', baseBps: 220 },
  { pairKey: 'CBTC/USDC', buyVenue: 'silvana', sellVenue: 'cantex', buyPrice: '95680', baseBps: 300 },
  { pairKey: 'CETH/USDC', buyVenue: 'silvana', sellVenue: 'temple', buyPrice: '3420', baseBps: 165 },
];

let liveCounter = 0;

function nextLiveSpread(): SpreadDto {
  const spec = LIVE_PAIRS[liveCounter % LIVE_PAIRS.length]!;
  liveCounter += 1;
  const jitter = Math.round((Math.sin(liveCounter * 1.7) + Math.random() * 0.6) * 18);
  const bps = Math.max(40, spec.baseBps + jitter);
  const buyPrice = Number(spec.buyPrice);
  const sellPrice = buyPrice * (1 + bps / 10_000);
  return {
    id: `sse-${Date.now()}-${liveCounter}`,
    ts: new Date().toISOString(),
    basePairKey: spec.pairKey,
    buyVenueId: spec.buyVenue,
    sellVenueId: spec.sellVenue,
    buyPrice: spec.buyPrice,
    sellPrice: sellPrice.toFixed(buyPrice >= 1 ? 2 : buyPrice >= 0.01 ? 8 : 12),
    spreadBps: bps,
    estProfitUsd: (bps / 100).toFixed(4),
    acted: false,
  };
}

/**
 * Subscribe to a synthetic live spread feed. Emits a fresh row every ~4 s from
 * a local interval — replaces the real SSE/EventSource entirely. Returns an
 * unsubscribe function.
 */
export function subscribeToSpreads(handlers: SpreadFeedHandlers): () => void {
  // mimic EventSource "open" asynchronously so the status dot flips to live
  const openTimer = setTimeout(() => handlers.onOpen?.(), 60);
  const id = setInterval(() => {
    handlers.onSpread(nextLiveSpread());
  }, 4000);
  return () => {
    clearTimeout(openTimer);
    clearInterval(id);
  };
}
