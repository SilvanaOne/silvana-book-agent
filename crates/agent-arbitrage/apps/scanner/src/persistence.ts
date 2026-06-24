/**
 * Scanner persistence layer.
 *
 * Optional — gated by env flag `SCANNER_PERSIST=1`. Without it, the scanner
 * runs purely in-memory (Sprint 1 default behaviour, no DB dependency).
 *
 * When enabled:
 *   1. On startup, idempotently seed Venue + Instrument + Market rows for
 *      the venues/pairs we currently scan.
 *   2. On each detected SpreadOpportunity, insert a row into the Spread table.
 *      `acted=false` always (paper mode); Opportunity / TradeAudit rows are
 *      written only when forwarding to the proprietary executor (Sprint 1+).
 *
 * Failures are logged and swallowed — DB outage must NOT crash the scanner.
 */

import { getDb, type PrismaClient } from '@arbitrage-agent/db';
import {
  type Pair,
  type SpreadOpportunity,
  type VenueId,
  tokenPairKey,
  createLogger,
} from '@arbitrage-agent/shared';
import type { DualTradeStatus, InventorySnapshot as InventorySnapshotDto } from '@arbitrage-agent/clients';

const log = createLogger('scanner.persistence');

export interface PersistenceContext {
  readonly db: PrismaClient;
}

export async function initPersistence(): Promise<PersistenceContext> {
  const db = getDb();
  // Connect lazily on first query; explicit ping verifies DB up at startup.
  await db.$queryRaw`SELECT 1`;
  return { db };
}

export async function getKillSwitch(ctx: PersistenceContext): Promise<boolean> {
  const row = await ctx.db.config.findUnique({ where: { key: 'runtime.killSwitch' } });
  if (!row) return false;
  const v = row.value as { enabled?: boolean };
  return Boolean(v?.enabled);
}

export interface VenueOutcome {
  readonly venueId: string;
  readonly ok: boolean;
  readonly error?: string;
}

/**
 * Record per-venue fetch outcome for the operator connection-status panel.
 * Streaks are incremented/reset so the UI can flag flapping venues.
 */
export async function recordVenueHealth(
  ctx: PersistenceContext,
  outcomes: readonly VenueOutcome[],
): Promise<void> {
  const now = new Date();
  for (const o of outcomes) {
    const prev = await ctx.db.venueHealth.findUnique({ where: { venueId: o.venueId } });
    if (o.ok) {
      await ctx.db.venueHealth.upsert({
        where: { venueId: o.venueId },
        create: { venueId: o.venueId, lastSuccessAt: now, successStreak: 1, errorStreak: 0 },
        update: { lastSuccessAt: now, successStreak: (prev?.successStreak ?? 0) + 1, errorStreak: 0 },
      });
    } else {
      await ctx.db.venueHealth.upsert({
        where: { venueId: o.venueId },
        create: {
          venueId: o.venueId,
          lastErrorAt: now,
          lastError: o.error ?? 'unknown',
          successStreak: 0,
          errorStreak: 1,
        },
        update: {
          lastErrorAt: now,
          lastError: o.error ?? 'unknown',
          successStreak: 0,
          errorStreak: (prev?.errorStreak ?? 0) + 1,
        },
      });
    }
  }
}

export async function shutdownPersistence(ctx: PersistenceContext): Promise<void> {
  await ctx.db.$disconnect();
}

/**
 * Seed Venue + Instrument + Market rows for the scanned pairs. Idempotent
 * — uses upsert so existing rows are left untouched.
 */
export async function ensureSeed(
  ctx: PersistenceContext,
  pairs: readonly Pair[],
): Promise<void> {
  const venueIds = new Set<VenueId>(pairs.map((p) => p.venueId));
  for (const venueId of venueIds) {
    await ctx.db.venue.upsert({
      where: { id: venueId },
      create: {
        id: venueId,
        type: venueType(venueId),
        displayName: humanize(venueId),
        enabled: true,
      },
      update: {},
    });
  }

  const instruments = new Map<string, { decimals: number; chainId: string }>();
  for (const pair of pairs) {
    instruments.set(pair.base.symbol, { decimals: pair.base.decimals, chainId: pair.base.chainId });
    instruments.set(pair.quote.symbol, {
      decimals: pair.quote.decimals,
      chainId: pair.quote.chainId,
    });
  }
  for (const [symbol, meta] of instruments) {
    await ctx.db.instrument.upsert({
      where: { id: symbol },
      create: { id: symbol, symbol, decimals: meta.decimals, chainId: meta.chainId },
      update: {},
    });
  }

  for (const pair of pairs) {
    await ctx.db.market.upsert({
      where: {
        venueId_baseId_quoteId: {
          venueId: pair.venueId,
          baseId: pair.base.symbol,
          quoteId: pair.quote.symbol,
        },
      },
      create: {
        venueId: pair.venueId,
        baseId: pair.base.symbol,
        quoteId: pair.quote.symbol,
      },
      update: {},
    });
  }
}

/** Persist a detected spread opportunity. Returns the new row id, or null on failure. */
export async function recordSpread(
  ctx: PersistenceContext,
  opp: SpreadOpportunity,
): Promise<bigint | null> {
  try {
    const row = await ctx.db.spread.create({
      data: {
        basePairKey: tokenPairKey(opp.pair),
        buyVenueId: opp.buyFrom.venueId,
        sellVenueId: opp.sellTo.venueId,
        buyPrice: opp.buyFrom.buyPrice.toString(),
        sellPrice: opp.sellTo.sellPrice.toString(),
        spreadBps: Math.round(opp.spreadPct * 100),
        estProfitUsd: opp.estimatedProfitUsd.toString(),
        acted: false,
        ts: new Date(opp.detectedAt),
      },
    });
    return row.id;
  } catch (err: unknown) {
    log.error({ err: err instanceof Error ? err.message : String(err) }, 'persist spread failed');
    return null;
  }
}

function humanize(venueId: VenueId): string {
  if (venueId === 'oneswap') return 'OneSwap';
  return venueId.charAt(0).toUpperCase() + venueId.slice(1);
}

/** Map a venue id to its Venue.type (see prisma schema VenueType). */
function venueType(venueId: VenueId): string {
  switch (venueId) {
    case 'oneswap':
    case 'cantex':
      return 'canton-amm';
    case 'silvana':
    case 'temple':
      return 'canton-clob';
    case 'hyperliquid':
      return 'perp-dex';
    default:
      return 'cex-spot';
  }
}

/** TradeState (executor) → TradeExecution.status enum. */
function execStatus(state: string): string {
  switch (state) {
    case 'executed':
      return 'SUCCESS';
    case 'partial':
      return 'PARTIAL';
    case 'skipped':
      return 'SKIPPED';
    default:
      return 'FAILED'; // failed | aborted | unexpected
  }
}

const bpsFromPct = (pct: number | undefined): number | null =>
  pct === undefined ? null : Math.round(pct * 100);

const tsDate = (ms: number | undefined): Date | null => (ms === undefined ? null : new Date(ms));

/**
 * Persist the executor's per-trade result (per-leg model). Writes one
 * TradeExecution row from the polled DualTradeStatus. Best-effort — failures
 * are swallowed so a DB hiccup never crashes the scan loop.
 */
export async function recordTradeExecution(
  ctx: PersistenceContext,
  status: DualTradeStatus,
  opp: SpreadOpportunity,
): Promise<void> {
  try {
    await ctx.db.tradeExecution.create({
      data: {
        executorTradeId: status.tradeId,
        ticker: tokenPairKey(opp.pair),
        buyVenueId: opp.buyFrom.venueId,
        sellVenueId: opp.sellTo.venueId,
        status: execStatus(status.status),
        partialExecution: status.partialExecution,
        detectedBuyPrice: status.detected?.buyPrice ?? opp.buyFrom.buyPrice,
        detectedSellPrice: status.detected?.sellPrice ?? opp.sellTo.sellPrice,
        spreadBps: Math.round(opp.spreadPct * 100),
        legBuyStatus: status.legBuy?.status ?? null,
        legBuyOrderId: status.legBuy?.orderId ?? null,
        legBuyQty: status.legBuy?.qty ?? null,
        legBuyUsdt: status.legBuy?.usdt ?? null,
        legBuyError: status.legBuy?.error ?? null,
        legSellStatus: status.legSell?.status ?? null,
        legSellOrderId: status.legSell?.orderId ?? null,
        legSellQty: status.legSell?.qty ?? null,
        legSellUsdt: status.legSell?.usdt ?? null,
        legSellError: status.legSell?.error ?? null,
        actualBuyPrice: status.actual?.buyPrice ?? null,
        actualSellPrice: status.actual?.sellPrice ?? null,
        buySlippageBps: bpsFromPct(status.actual?.buySlippagePct),
        sellSlippageBps: bpsFromPct(status.actual?.sellSlippagePct),
        profitUsdtDiff: status.profit?.usdtDiff ?? null,
        profitTokenPct: status.profit?.tokenPct ?? null,
        buyExecutedAt: tsDate(status.timing?.buyExecutedAt),
        sellExecutedAt: tsDate(status.timing?.sellExecutedAt),
        totalExecMs: status.timing?.totalExecutionMs ?? null,
        failReason: status.failReason ?? null,
      },
    });
  } catch (err: unknown) {
    log.error({ err: err instanceof Error ? err.message : String(err) }, 'persist trade execution failed');
  }
}

/** Persist a set of inventory snapshots polled from the rebalancer. */
export async function recordInventory(
  ctx: PersistenceContext,
  snapshots: readonly InventorySnapshotDto[],
): Promise<void> {
  if (snapshots.length === 0) return;
  try {
    await ctx.db.inventorySnapshot.createMany({
      data: snapshots.map((s) => ({ venueId: s.venue, asset: s.asset, free: s.free, locked: s.locked })),
    });
  } catch (err: unknown) {
    log.error({ err: err instanceof Error ? err.message : String(err) }, 'persist inventory failed');
  }
}
