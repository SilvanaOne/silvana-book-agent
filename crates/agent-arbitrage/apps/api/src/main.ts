/**
 * API server bootstrap — Express + Prisma DB + SpreadFeed singleton + auth.
 *
 * Sprint 2.3: HMAC bearer tokens + RSA-encrypted login.
 * Sprint 2.4: DB-backed config + kill switch.
 */

import 'dotenv/config';
import { createLogger } from '@arbitrage-agent/shared';
import { getDb, disconnectDb } from '@arbitrage-agent/db';
import { createServer } from './server.js';
import { SpreadFeed } from './spreadFeed.js';
import { bootstrapAuth } from './auth/bootstrap.js';
import { startBalanceRefreshLoop } from './balances/refresh.js';

const log = createLogger('api');

const PORT = Number(process.env['PORT'] ?? 3000);
const HOST = process.env['HOST'] ?? '0.0.0.0';
const FEED_POLL_MS = Number(process.env['FEED_POLL_MS'] ?? 500);

async function main(): Promise<void> {
  // Ensure DB is reachable on startup so /api/spreads doesn't 500 on first call.
  try {
    await getDb().$queryRaw`SELECT 1`;
    log.info('db connection ok');
  } catch (err: unknown) {
    log.warn(
      { err: err instanceof Error ? err.message : String(err) },
      'db connection failed — /api/spreads will error until DB is reachable',
    );
  }

  try {
    await bootstrapAuth();
  } catch (err: unknown) {
    log.warn(
      { err: err instanceof Error ? err.message : String(err) },
      'auth bootstrap failed — /api/auth/* will error until DB is reachable',
    );
  }

  const spreadFeed = new SpreadFeed();
  await spreadFeed.start(FEED_POLL_MS);
  startBalanceRefreshLoop();

  const app = createServer({ spreadFeed });
  const server = app.listen(PORT, HOST, () => {
    log.info({ host: HOST, port: PORT }, 'api listening');
  });

  const shutdown = async (signal: string): Promise<void> => {
    log.info({ signal }, 'shutting down');
    spreadFeed.stop();
    await new Promise<void>((resolve) => server.close(() => resolve()));
    await disconnectDb();
    process.exit(0);
  };
  process.on('SIGINT', () => void shutdown('SIGINT'));
  process.on('SIGTERM', () => void shutdown('SIGTERM'));
}

main().catch((err: unknown) => {
  log.fatal({ err: err instanceof Error ? err.message : String(err) }, 'fatal');
  process.exit(1);
});
