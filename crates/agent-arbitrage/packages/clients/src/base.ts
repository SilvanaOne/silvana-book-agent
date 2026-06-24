/**
 * Base for the typed proprietary-engine HTTP clients.
 *
 * Open core only: builds + signs requests and parses typed responses. Holds no
 * keys. The engines it talks to live in separate private repos; until they are
 * reachable, callers should treat failures as "execution disabled" and degrade
 * gracefully (detection keeps running).
 */

import { fetchJson } from '@arbitrage-agent/shared';
import { authHeaders } from './auth.js';

export interface EngineClientConfig {
  /** Base URL, e.g. http://localhost:4001 (no trailing slash). */
  readonly baseUrl: string;
  /** Shared HMAC secret for this engine (from .env). */
  readonly token: string;
  /** Per-request timeout in ms. Default 8000. */
  readonly timeoutMs?: number;
  /** Retry attempts on transient failures. Default 2. */
  readonly maxRetries?: number;
  /** Injectable clock for deterministic tests. */
  readonly now?: () => number;
}

export class EngineClientError extends Error {
  constructor(
    message: string,
    readonly engine: string,
    readonly cause?: unknown,
  ) {
    super(message);
    this.name = 'EngineClientError';
  }
}

export abstract class BaseEngineClient {
  protected abstract readonly engineName: string;

  constructor(protected readonly cfg: EngineClientConfig) {
    if (!cfg.baseUrl) throw new Error('EngineClientConfig.baseUrl is required');
  }

  protected async request<T>(method: string, path: string, body?: unknown): Promise<T> {
    const bodyStr = body === undefined ? '' : JSON.stringify(body);
    // HMAC covers pathname only — query params are not part of the signed path (vault/engines use req.path).
    const signPath = path.split('?')[0] ?? path;
    const headers: Record<string, string> = {
      ...(bodyStr ? { 'Content-Type': 'application/json' } : {}),
      ...authHeaders(this.cfg.token, method, signPath, bodyStr, this.cfg.now),
    };
    try {
      return await fetchJson<T>(`${this.cfg.baseUrl}${path}`, {
        method,
        ...(bodyStr ? { body: bodyStr } : {}),
        headers,
        timeoutMs: this.cfg.timeoutMs ?? 8000,
        maxRetries: this.cfg.maxRetries ?? 2,
      });
    } catch (err) {
      throw new EngineClientError(`${this.engineName} ${method} ${path} failed: ${String(err)}`, this.engineName, err);
    }
  }

  /** Returns true if the engine is reachable and healthy. Never throws. */
  async isHealthy(): Promise<boolean> {
    try {
      const h = await this.request<{ status: string }>('GET', '/health');
      return h.status === 'ok' || h.status === 'degraded';
    } catch {
      return false;
    }
  }
}
