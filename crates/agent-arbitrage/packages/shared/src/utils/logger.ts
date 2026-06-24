/**
 * Structured logging via Pino.
 *
 * One logger per subsystem name (`scanner`, `api`, `bybit`, etc.) so the
 * `name` field segments lines for downstream parsing. Level via env
 * LOG_LEVEL (default `info`).
 *
 * For human-friendly output in dev: pipe stdout through `npx pino-pretty`.
 */

import { pino, type Logger } from 'pino';

const LOG_LEVEL = process.env['LOG_LEVEL'] ?? 'info';

export function createLogger(name: string): Logger {
  return pino({
    name,
    level: LOG_LEVEL,
    base: null, // omit pid/hostname — they bloat structured output
    timestamp: pino.stdTimeFunctions.isoTime,
  });
}

export type { Logger } from 'pino';
