/**
 * Stateless HMAC-signed session tokens.
 *
 * Format: `<header_b64>.<payload_b64>.<sig_b64>` (JWT-like but tiny — we don't
 * need full JWT compatibility, just signed envelopes).
 *
 * Signing key comes from `TOKEN_SECRET`. If unset, a per-process random key
 * is used (every restart invalidates all sessions — fine for dev, never use
 * in production; we log a warning).
 */

import { createHmac, randomBytes, timingSafeEqual } from 'node:crypto';
import { createLogger } from '@arbitrage-agent/shared';

const log = createLogger('api.auth.token');

export interface TokenPayload {
  readonly sub: string; // operator id
  readonly username: string;
  readonly iat: number; // seconds since epoch
  readonly exp: number; // seconds since epoch
}

const DEFAULT_TTL_SECONDS = 60 * 60; // 1 hour

let cachedSecret: Buffer | null = null;

function getSecret(): Buffer {
  if (cachedSecret) return cachedSecret;
  const raw = process.env['TOKEN_SECRET'];
  if (raw && raw.length >= 32) {
    cachedSecret = Buffer.from(raw, 'utf8');
  } else {
    cachedSecret = randomBytes(32);
    log.warn(
      'TOKEN_SECRET not set or too short — using ephemeral per-process secret (all sessions invalidate on restart)',
    );
  }
  return cachedSecret;
}

function b64url(buf: Buffer): string {
  return buf.toString('base64').replace(/=+$/, '').replace(/\+/g, '-').replace(/\//g, '_');
}

function fromB64url(s: string): Buffer {
  return Buffer.from(s.replace(/-/g, '+').replace(/_/g, '/'), 'base64');
}

export function signToken(
  payload: Omit<TokenPayload, 'iat' | 'exp'>,
  ttlSeconds: number = DEFAULT_TTL_SECONDS,
): { token: string; expiresAt: number } {
  const now = Math.floor(Date.now() / 1000);
  const full: TokenPayload = { ...payload, iat: now, exp: now + ttlSeconds };
  const header = b64url(Buffer.from(JSON.stringify({ alg: 'HS256', typ: 'OPCORE' })));
  const body = b64url(Buffer.from(JSON.stringify(full)));
  const sig = b64url(createHmac('sha256', getSecret()).update(`${header}.${body}`).digest());
  return { token: `${header}.${body}.${sig}`, expiresAt: full.exp * 1000 };
}

export type VerifyResult =
  | { ok: true; payload: TokenPayload }
  | { ok: false; reason: 'malformed' | 'bad_signature' | 'expired' };

export function verifyToken(token: string): VerifyResult {
  const parts = token.split('.');
  if (parts.length !== 3) return { ok: false, reason: 'malformed' };
  const [header, body, sig] = parts as [string, string, string];

  const expected = createHmac('sha256', getSecret()).update(`${header}.${body}`).digest();
  let provided: Buffer;
  try {
    provided = fromB64url(sig);
  } catch {
    return { ok: false, reason: 'malformed' };
  }
  if (provided.length !== expected.length || !timingSafeEqual(provided, expected)) {
    return { ok: false, reason: 'bad_signature' };
  }

  let payload: TokenPayload;
  try {
    payload = JSON.parse(fromB64url(body).toString('utf8')) as TokenPayload;
  } catch {
    return { ok: false, reason: 'malformed' };
  }

  if (typeof payload.exp !== 'number' || payload.exp * 1000 < Date.now()) {
    return { ok: false, reason: 'expired' };
  }
  return { ok: true, payload };
}
