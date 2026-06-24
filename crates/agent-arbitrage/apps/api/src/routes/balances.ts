import { Router, type Request, type Response } from 'express';
import { getCachedBalances } from '../balances/cache.js';

export const balancesRouter = Router();

/** Always returns the in-memory cache — never blocks on CEX/vault fetches. */
balancesRouter.get('/', (_req: Request, res: Response) => {
  res.json(getCachedBalances());
});
