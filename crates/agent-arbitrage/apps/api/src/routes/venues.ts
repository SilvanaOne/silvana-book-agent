/**
 * Per-venue connection health — derived from the VenueHealth table the
 * scanner upserts each cycle.
 *
 * Status is computed from freshness + error streak:
 *   healthy — last success within HEALTHY_MS
 *   stale   — last success within STALE_MS (degraded but recently alive)
 *   down    — no success within STALE_MS, or never seen
 */

import { Router, type Request, type Response } from 'express';
import { getDb } from '@arbitrage-agent/db';

export const venuesRouter = Router();

const HEALTHY_MS = 30_000;
const STALE_MS = 5 * 60_000;

type VenueStatus = 'healthy' | 'stale' | 'down';

function computeStatus(lastSuccessAt: Date | null): VenueStatus {
  if (!lastSuccessAt) return 'down';
  const age = Date.now() - lastSuccessAt.getTime();
  if (age <= HEALTHY_MS) return 'healthy';
  if (age <= STALE_MS) return 'stale';
  return 'down';
}

venuesRouter.get('/health', async (_req: Request, res: Response) => {
  const [venues, health] = await Promise.all([
    getDb().venue.findMany({ orderBy: { id: 'asc' } }),
    getDb().venueHealth.findMany(),
  ]);
  const healthById = new Map(health.map((h) => [h.venueId, h]));

  const rows = venues.map((v) => {
    const h = healthById.get(v.id);
    return {
      venueId: v.id,
      displayName: v.displayName,
      enabled: v.enabled,
      status: computeStatus(h?.lastSuccessAt ?? null),
      lastSuccessAt: h?.lastSuccessAt?.toISOString() ?? null,
      lastErrorAt: h?.lastErrorAt?.toISOString() ?? null,
      lastError: h?.lastError ?? null,
      successStreak: h?.successStreak ?? 0,
      errorStreak: h?.errorStreak ?? 0,
    };
  });

  res.json({ venues: rows });
});
