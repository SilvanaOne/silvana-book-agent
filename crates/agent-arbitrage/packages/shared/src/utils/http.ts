/**
 * Minimal HTTP helper for venue clients.
 *
 * Uses Node 20+ global fetch with AbortController for timeout. No external deps.
 * Designed for public REST endpoints (no auth) — auth-bearing requests live in
 * proprietary executor / rebalancer, never in open-core.
 *
 * Includes retry-with-exponential-backoff for transient failures (network
 * errors + HTTP 5xx). HTTP 4xx is treated as fatal — no retry.
 */

export class HttpError extends Error {
  constructor(
    message: string,
    readonly status: number,
    readonly body: string,
  ) {
    super(message);
    this.name = 'HttpError';
  }
}

export interface FetchJsonOptions {
  /** HTTP method. Default 'GET'. */
  readonly method?: string;
  /** Request body (already serialized, e.g. JSON.stringify(...)). */
  readonly body?: string;
  /** Per-attempt timeout in ms. Default 10s. */
  readonly timeoutMs?: number;
  /** Extra headers. */
  readonly headers?: Record<string, string>;
  /** AbortSignal — cooperative cancellation from caller. */
  readonly signal?: AbortSignal;
  /** Maximum retry attempts on transient failures. Default 3. 0 disables retry. */
  readonly maxRetries?: number;
  /** Base delay for exponential backoff (ms). Default 200ms. */
  readonly retryBaseMs?: number;
  /** Optional hook fired before each retry — useful for logging upstream flakiness. */
  readonly onRetry?: (attempt: number, err: unknown) => void;
}

function isRetryable(err: unknown): boolean {
  if (err instanceof HttpError) {
    return err.status >= 500 && err.status <= 599;
  }
  // Network errors (DNS, TLS, ECONNREFUSED, AbortError on timeout, etc.) — retry.
  return true;
}

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

async function fetchJsonOnce<T>(
  url: string,
  options: Omit<FetchJsonOptions, 'maxRetries' | 'retryBaseMs' | 'onRetry'>,
): Promise<T> {
  const { method = 'GET', body: requestBody, timeoutMs = 10_000, headers, signal: callerSignal } = options;
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);

  const cleanupCallerAbort = callerSignal
    ? (() => {
        const handler = (): void => controller.abort();
        callerSignal.addEventListener('abort', handler, { once: true });
        return () => callerSignal.removeEventListener('abort', handler);
      })()
    : undefined;

  try {
    const response = await fetch(url, {
      method,
      ...(requestBody !== undefined ? { body: requestBody } : {}),
      headers: { Accept: 'application/json', ...(headers ?? {}) },
      signal: controller.signal,
    });

    const body = await response.text();
    if (!response.ok) {
      throw new HttpError(`HTTP ${response.status} for ${url}`, response.status, body);
    }
    // 204 No Content / empty body — valid "void" response (e.g. signer saveSecret).
    if (body === '') {
      return undefined as T;
    }
    try {
      return JSON.parse(body) as T;
    } catch {
      throw new HttpError(`Invalid JSON from ${url}`, response.status, body);
    }
  } finally {
    clearTimeout(timer);
    cleanupCallerAbort?.();
  }
}

export async function fetchJson<T>(url: string, options: FetchJsonOptions = {}): Promise<T> {
  const maxRetries = options.maxRetries ?? 3;
  const baseMs = options.retryBaseMs ?? 200;
  const totalAttempts = maxRetries + 1;

  let lastErr: unknown;
  for (let attempt = 0; attempt < totalAttempts; attempt++) {
    try {
      return await fetchJsonOnce<T>(url, options);
    } catch (err) {
      lastErr = err;
      const isLastAttempt = attempt === totalAttempts - 1;
      if (isLastAttempt || !isRetryable(err)) {
        throw err;
      }
      options.onRetry?.(attempt + 1, err);
      // Exponential backoff with 0-100ms jitter
      const delay = baseMs * 2 ** attempt + Math.floor(Math.random() * 100);
      await sleep(delay);
    }
  }
  throw lastErr; // unreachable, satisfies TS
}
