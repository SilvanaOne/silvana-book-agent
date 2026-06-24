import type { Request, Response, NextFunction } from 'express';
import { verifyToken, type TokenPayload } from './token.js';

declare module 'express-serve-static-core' {
  interface Request {
    operator?: TokenPayload;
  }
}

function extractToken(req: Request): string | null {
  const h = req.header('authorization') ?? req.header('Authorization');
  if (h && h.startsWith('Bearer ')) return h.slice('Bearer '.length).trim();
  // EventSource cannot set headers — accept `?token=` as fallback for SSE.
  const q = req.query['token'];
  if (typeof q === 'string') return q;
  return null;
}

export function requireAuth(req: Request, res: Response, next: NextFunction): void {
  if (process.env['DISABLE_AUTH'] === '1') {
    next();
    return;
  }
  const tok = extractToken(req);
  if (!tok) {
    res.status(401).json({ error: 'unauthenticated' });
    return;
  }
  const result = verifyToken(tok);
  if (!result.ok) {
    res.status(401).json({ error: 'unauthenticated', reason: result.reason });
    return;
  }
  req.operator = result.payload;
  next();
}
