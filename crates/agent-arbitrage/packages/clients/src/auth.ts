/**
 * Request signing for open-core → proprietary-engine calls.
 *
 * Every request is signed with the per-engine shared secret (from `.env`,
 * e.g. EXECUTOR_API_TOKEN) using HMAC-SHA256 over a canonical string:
 *
 *     ${timestamp}\n${METHOD}\n${path}\n${body}
 *
 * The signature travels in `X-Arb-Signature: sha256=<hex>` alongside
 * `X-Arb-Timestamp`. The engine recomputes the HMAC and rejects on mismatch or
 * stale timestamp. Transport security (mTLS) is layered on top in production
 * and is out of scope here. See md_docs/12 §"Контракты между компонентами".
 *
 * This is open-core: it only describes how to AUTHENTICATE a request. It holds
 * no exchange/Canton keys — those live in the signer vault + engines.
 */

import { createHmac } from 'node:crypto';

export const TIMESTAMP_HEADER = 'X-Arb-Timestamp';
export const SIGNATURE_HEADER = 'X-Arb-Signature';

/** Canonical string that both sides sign. */
export function canonicalString(timestamp: string, method: string, path: string, body: string): string {
  return `${timestamp}\n${method.toUpperCase()}\n${path}\n${body}`;
}

/** HMAC-SHA256 hex signature of the canonical request. */
export function signRequest(secret: string, timestamp: string, method: string, path: string, body: string): string {
  return createHmac('sha256', secret).update(canonicalString(timestamp, method, path, body)).digest('hex');
}

/** Build the auth headers for a request. `now` is injectable for deterministic tests. */
export function authHeaders(
  secret: string,
  method: string,
  path: string,
  body: string,
  now: () => number = Date.now,
): Record<string, string> {
  const timestamp = String(now());
  return {
    [TIMESTAMP_HEADER]: timestamp,
    [SIGNATURE_HEADER]: `sha256=${signRequest(secret, timestamp, method, path, body)}`,
  };
}
