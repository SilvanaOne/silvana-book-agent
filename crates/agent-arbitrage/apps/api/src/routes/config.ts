import { Router, type Request, type Response } from 'express';
import {
  getRuntimeConfig,
  updateTradeConfig,
  updateRiskConfig,
  setKillSwitch,
  type TradeConfigDto,
  type RiskConfigDto,
} from '../lib/configStore.js';
import { requireAuth } from '../auth/middleware.js';

export const configRouter = Router();

configRouter.get('/', async (_req: Request, res: Response) => {
  const cfg = await getRuntimeConfig();
  res.json(cfg);
});

function parseNum(v: unknown): number | null {
  if (typeof v !== 'number' || !Number.isFinite(v)) return null;
  return v;
}

type Mutable<T> = { -readonly [K in keyof T]: T[K] };

configRouter.put('/trade', requireAuth, async (req: Request, res: Response) => {
  const body = (req.body ?? {}) as Record<string, unknown>;
  const patch: Partial<Mutable<TradeConfigDto>> = {};
  for (const k of ['targetSpreadPercent', 'maxSpreadPercent', 'tradeSizeUsd'] as const) {
    if (k in body) {
      const n = parseNum(body[k]);
      if (n === null || n < 0) {
        res.status(400).json({ error: 'bad_request', field: k });
        return;
      }
      patch[k] = n;
    }
  }
  const next = await updateTradeConfig(patch, `operator:${req.operator?.username ?? 'unknown'}`);
  res.json(next);
});

configRouter.put('/risk', requireAuth, async (req: Request, res: Response) => {
  const body = (req.body ?? {}) as Record<string, unknown>;
  const patch: Partial<Mutable<RiskConfigDto>> = {};
  for (const k of [
    'maxSpreadBps',
    'maxQuoteAgeMs',
    'dailyLossLimitUsd',
    'maxConsecutiveLosses',
  ] as const) {
    if (k in body) {
      const n = parseNum(body[k]);
      if (n === null || n < 0) {
        res.status(400).json({ error: 'bad_request', field: k });
        return;
      }
      patch[k] = n;
    }
  }
  const next = await updateRiskConfig(patch, `operator:${req.operator?.username ?? 'unknown'}`);
  res.json(next);
});

configRouter.post('/kill-switch', requireAuth, async (req: Request, res: Response) => {
  const body = (req.body ?? {}) as { enabled?: unknown };
  if (typeof body.enabled !== 'boolean') {
    res.status(400).json({ error: 'bad_request', field: 'enabled' });
    return;
  }
  const next = await setKillSwitch(body.enabled, `operator:${req.operator?.username ?? 'unknown'}`);
  res.json({ killSwitch: next });
});
