import { Router, type Request, type Response } from 'express';
import { createLogger } from '@arbitrage-agent/shared';
import type { SpreadFeed } from '../spreadFeed.js';
import type { SpreadDto } from '../lib/spreadDto.js';

const log = createLogger('api.events');
const HEARTBEAT_MS = 30_000;

export function createEventsRouter(feed: SpreadFeed): Router {
  const router = Router();

  router.get('/stream', (req: Request, res: Response) => {
    res.setHeader('Content-Type', 'text/event-stream');
    res.setHeader('Cache-Control', 'no-cache, no-transform');
    res.setHeader('Connection', 'keep-alive');
    res.setHeader('X-Accel-Buffering', 'no'); // disable nginx response buffering
    res.flushHeaders();

    // Opening comment line — some EventSource impls need any byte to confirm
    // the connection is alive before the first real event.
    res.write(`: connected\n\n`);

    const onSpread = (s: SpreadDto): void => {
      res.write(`event: spread\ndata: ${JSON.stringify(s)}\n\n`);
    };
    feed.on('spread', onSpread);

    const hb = setInterval(() => {
      res.write(`event: heartbeat\ndata: ${JSON.stringify({ ts: new Date().toISOString() })}\n\n`);
    }, HEARTBEAT_MS);

    log.info('client connected');

    const cleanup = (): void => {
      feed.off('spread', onSpread);
      clearInterval(hb);
      log.info('client disconnected');
    };
    req.on('close', cleanup);
    req.on('aborted', cleanup);
  });

  return router;
}
