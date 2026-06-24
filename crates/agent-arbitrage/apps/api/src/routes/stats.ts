/**
 * Aggregate stats over recent spreads + a lightweight P&L summary.
 *
 * P&L is "estimated" only until the proprietary executor wires in: we sum
 * estProfitUsd over spreads marked `acted`, plus realizedProfitUsd from any
 * Opportunity rows the executor has reported back. With paper mode both are
 * effectively zero — the panel shows the shape, not live money.
 */

import { Router, type Request, type Response } from 'express';
import { getDb } from '@arbitrage-agent/db';

export const statsRouter = Router();

const WINDOW_MS = 60 * 60_000; // 1 hour

statsRouter.get('/', async (_req: Request, res: Response) => {
  const since = new Date(Date.now() - WINDOW_MS);
  const db = getDb();

  const [total, windowed, agg, actedAgg, realizedAgg] = await Promise.all([
    db.spread.count(),
    db.spread.count({ where: { ts: { gte: since } } }),
    db.spread.aggregate({
      where: { ts: { gte: since } },
      _avg: { spreadBps: true },
      _max: { spreadBps: true },
    }),
    db.spread.aggregate({ where: { acted: true }, _sum: { estProfitUsd: true }, _count: true }),
    db.opportunity.aggregate({ _sum: { realizedProfitUsd: true } }),
  ]);

  // Group recent spreads by route (buy → sell venue) for a quick breakdown.
  const byRoute = await db.spread.groupBy({
    by: ['buyVenueId', 'sellVenueId'],
    where: { ts: { gte: since } },
    _count: true,
  });

  res.json({
    windowMs: WINDOW_MS,
    spreads: {
      total,
      lastHour: windowed,
      avgBps: agg._avg.spreadBps ? Math.round(agg._avg.spreadBps) : 0,
      maxBps: agg._max.spreadBps ?? 0,
      byRoute: byRoute.map((r) => ({
        route: `${r.buyVenueId} → ${r.sellVenueId}`,
        count: r._count,
      })),
    },
    pnl: {
      actedCount: actedAgg._count,
      estimatedUsd: actedAgg._sum.estProfitUsd?.toString() ?? '0',
      realizedUsd: realizedAgg._sum.realizedProfitUsd?.toString() ?? '0',
    },
  });
});

/** Spread-size histogram buckets (bps), small → large. */
const SIZE_BUCKETS: ReadonlyArray<{ label: string; min: number; max: number | null }> = [
  { label: '<50', min: 0, max: 50 },
  { label: '50–100', min: 50, max: 100 },
  { label: '100–200', min: 100, max: 200 },
  { label: '200–300', min: 200, max: 300 },
  { label: '300–500', min: 300, max: 500 },
  { label: '500+', min: 500, max: null },
];

/**
 * Profitability analytics for the Stats tab: per-venue spread participation +
 * attributed estimated profit, and the spread-size distribution. All-time
 * (not windowed) so the charts have enough mass to be meaningful.
 */
statsRouter.get('/profitability', async (_req: Request, res: Response) => {
  const db = getDb();

  const [buyGroups, sellGroups, sizeCounts, totals] = await Promise.all([
    db.spread.groupBy({ by: ['buyVenueId'], _count: true, _sum: { estProfitUsd: true } }),
    db.spread.groupBy({ by: ['sellVenueId'], _count: true }),
    Promise.all(
      SIZE_BUCKETS.map((b) =>
        db.spread.count({
          where: { spreadBps: b.max === null ? { gte: b.min } : { gte: b.min, lt: b.max } },
        }),
      ),
    ),
    db.spread.aggregate({
      _count: true,
      _sum: { estProfitUsd: true },
      _avg: { spreadBps: true },
      _max: { spreadBps: true },
    }),
  ]);

  const venues = new Map<
    string,
    { venueId: string; buyCount: number; sellCount: number; estProfitUsd: number }
  >();
  const bump = (id: string): { venueId: string; buyCount: number; sellCount: number; estProfitUsd: number } => {
    let v = venues.get(id);
    if (!v) {
      v = { venueId: id, buyCount: 0, sellCount: 0, estProfitUsd: 0 };
      venues.set(id, v);
    }
    return v;
  };
  for (const g of buyGroups) {
    const v = bump(g.buyVenueId);
    v.buyCount = g._count;
    v.estProfitUsd += Number(g._sum.estProfitUsd ?? 0);
  }
  for (const g of sellGroups) {
    bump(g.sellVenueId).sellCount = g._count;
  }

  res.json({
    totals: {
      count: totals._count,
      estProfitUsd: totals._sum.estProfitUsd?.toString() ?? '0',
      avgBps: totals._avg.spreadBps ? Math.round(totals._avg.spreadBps) : 0,
      maxBps: totals._max.spreadBps ?? 0,
    },
    byVenue: [...venues.values()]
      .map((v) => ({
        venueId: v.venueId,
        count: v.buyCount + v.sellCount,
        buyCount: v.buyCount,
        sellCount: v.sellCount,
        estProfitUsd: v.estProfitUsd.toFixed(4),
      }))
      .sort((a, b) => b.count - a.count),
    bySize: SIZE_BUCKETS.map((b, i) => ({
      label: b.label,
      min: b.min,
      max: b.max,
      count: sizeCounts[i] ?? 0,
    })),
  });
});
