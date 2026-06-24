/** HMAC verification — mirrors open-core packages/clients/auth.ts. */

import { createHmac, timingSafeEqual } from 'node:crypto';
import type { NextFunction, Request, Response } from 'express';
import type { Config } from './config.js';

export interface RawBodyRequest extends Request {
  rawBody?: string;
}

function expected(secret: string, ts: string, method: string, path: string, body: string): string {
  return createHmac('sha256', secret).update(`${ts}\n${method.toUpperCase()}\n${path}\n${body}`).digest('hex');
}

function safeEqualHex(a: string, b: string): boolean {
  if (a.length !== b.length) return false;
  try {
    return timingSafeEqual(Buffer.from(a, 'hex'), Buffer.from(b, 'hex'));
  } catch {
    return false;
  }
}

export function hmacAuth(cfg: Config) {
  return (req: RawBodyRequest, res: Response, next: NextFunction): void => {
    const ts = req.header('x-arb-timestamp');
    const sig = req.header('x-arb-signature');
    if (!ts || !sig) {
      res.status(401).json({ error: 'missing auth headers' });
      return;
    }
    if (Math.abs(Date.now() - Number(ts)) / 1000 > cfg.maxSkewSec) {
      res.status(401).json({ error: 'stale or invalid timestamp' });
      return;
    }
    // originalUrl (not req.path) — req.path is stripped of the '/ai' mount prefix
    // inside app.use('/ai', …), but the client signs the full path.
    const fullPath = (req.originalUrl || req.url).split('?')[0] ?? '';
    if (!safeEqualHex(sig.replace(/^sha256=/, ''), expected(cfg.apiToken, ts, req.method, fullPath, req.rawBody ?? ''))) {
      res.status(401).json({ error: 'bad signature' });
      return;
    }
    next();
  };
}
