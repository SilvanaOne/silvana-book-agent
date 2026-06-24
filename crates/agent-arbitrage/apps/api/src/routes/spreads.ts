import { Router, type Request, type Response } from 'express';
import { getDb } from '@arbitrage-agent/db';
import { spreadRowToDto } from '../lib/spreadDto.js';

export const spreadsRouter = Router();

spreadsRouter.get('/', async (req: Request, res: Response) => {
  const rawLimit = Number(req.query['limit'] ?? 100);
  const limit = Math.min(Math.max(1, Number.isFinite(rawLimit) ? rawLimit : 100), 500);

  const rows = await getDb().spread.findMany({
    take: limit,
    orderBy: { ts: 'desc' },
  });

  const dtos = rows.map(spreadRowToDto);

  res.json({ spreads: dtos, count: dtos.length, limit });
});
