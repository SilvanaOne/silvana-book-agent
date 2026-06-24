import express, { type Express, type NextFunction, type Request, type Response } from 'express';
import cors from 'cors';
import helmet from 'helmet';
import { createLogger } from '@arbitrage-agent/shared';
import { spreadsRouter } from './routes/spreads.js';
import { configRouter } from './routes/config.js';
import { authRouter } from './routes/auth.js';
import { venuesRouter } from './routes/venues.js';
import { statsRouter } from './routes/stats.js';
import { activityRouter } from './routes/activity.js';
import { createEventsRouter } from './routes/events.js';
import { signerRouter } from './routes/signer.js';
import { executionRouter } from './routes/execution.js';
import { balancesRouter } from './routes/balances.js';
import { requireAuth } from './auth/middleware.js';
import type { SpreadFeed } from './spreadFeed.js';

const log = createLogger('api');

export interface ServerDeps {
  readonly spreadFeed: SpreadFeed;
}

export function createServer(deps: ServerDeps): Express {
  const app = express();

  app.use(helmet());
  app.use(cors());
  app.use(express.json({ limit: '1mb' }));

  // Public endpoints — health probe + login flow.
  app.get('/api/health', (_req, res) => {
    res.json({ status: 'ok', timestamp: new Date().toISOString() });
  });
  app.use('/api/auth', authRouter);

  // Everything below requires a valid bearer token (or `?token=` for SSE).
  // Dev override: DISABLE_AUTH=1 short-circuits the middleware (logged warn
  // in token.ts when TOKEN_SECRET is missing).
  app.use('/api/spreads', requireAuth, spreadsRouter);
  app.use('/api/config', configRouter); // GET public; PUT/POST inside use requireAuth
  app.use('/api/venues', requireAuth, venuesRouter);
  app.use('/api/stats', requireAuth, statsRouter);
  app.use('/api/activity', requireAuth, activityRouter);
  app.use('/api/events', requireAuth, createEventsRouter(deps.spreadFeed));
  app.use('/api/signer', requireAuth, signerRouter);
  app.use('/api/execution', requireAuth, executionRouter);
  app.use('/api/balances', requireAuth, balancesRouter);

  app.use((err: unknown, req: Request, res: Response, _next: NextFunction) => {
    log.error(
      { err: err instanceof Error ? err.message : String(err), path: req.path },
      'unhandled error',
    );
    if (!res.headersSent) {
      res.status(500).json({ error: 'internal_error' });
    }
  });

  return app;
}
