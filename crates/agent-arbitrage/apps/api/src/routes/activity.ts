/**
 * Read-only activity feeds for the operator UI:
 *   GET /api/activity/audit     — operator + system actions (kill switch, config)
 *   GET /api/activity/trades    — TradeAudit events (empty until executor wires)
 *   GET /api/activity/inventory — latest InventorySnapshot per venue+asset
 */

import { Router, type Request, type Response } from 'express';
import { getDb } from '@arbitrage-agent/db';

export const activityRouter = Router();

function clampLimit(raw: unknown, def = 50, max = 200): number {
  const n = Number(raw ?? def);
  return Math.min(Math.max(1, Number.isFinite(n) ? n : def), max);
}

activityRouter.get('/audit', async (req: Request, res: Response) => {
  const limit = clampLimit(req.query['limit']);
  const rows = await getDb().auditLog.findMany({ take: limit, orderBy: { id: 'desc' } });
  res.json({
    entries: rows.map((r) => ({
      id: r.id.toString(),
      ts: r.ts.toISOString(),
      actor: r.actor,
      action: r.action,
      payload: r.payload,
    })),
  });
});

activityRouter.get('/trades', async (req: Request, res: Response) => {
  const limit = clampLimit(req.query['limit']);
  // U4: per-trade execution summaries reported by the proprietary executor
  // (one row per dual-trade; status SUCCESS|PARTIAL|FAILED|SKIPPED).
  const rows = await getDb().tradeExecution.findMany({ take: limit, orderBy: { id: 'desc' } });
  res.json({
    trades: rows.map((r) => ({
      id: r.id.toString(),
      ts: r.ts.toISOString(),
      executorTradeId: r.executorTradeId,
      ticker: r.ticker,
      buyVenueId: r.buyVenueId,
      sellVenueId: r.sellVenueId,
      status: r.status,
      partialExecution: r.partialExecution,
      profitUsdtDiff: r.profitUsdtDiff ? r.profitUsdtDiff.toString() : null,
      spreadBps: r.spreadBps,
      legBuyStatus: r.legBuyStatus,
      legSellStatus: r.legSellStatus,
      failReason: r.failReason,
    })),
  });
});

activityRouter.get('/inventory', async (_req: Request, res: Response) => {
  // Latest snapshot per (venueId, asset). Pull a recent slice and dedupe in
  // memory — snapshot volume is low (one set per rebalancer poll).
  const rows = await getDb().inventorySnapshot.findMany({
    take: 500,
    orderBy: { ts: 'desc' },
  });
  const seen = new Set<string>();
  const latest: typeof rows = [];
  for (const r of rows) {
    const key = `${r.venueId}:${r.asset}`;
    if (seen.has(key)) continue;
    seen.add(key);
    latest.push(r);
  }
  res.json({
    holdings: latest.map((r) => ({
      venueId: r.venueId,
      asset: r.asset,
      free: r.free.toString(),
      locked: r.locked.toString(),
      ts: r.ts.toISOString(),
    })),
  });
});
