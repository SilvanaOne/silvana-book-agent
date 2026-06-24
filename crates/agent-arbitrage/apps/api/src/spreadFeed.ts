/**
 * SpreadFeed — single shared polling source for live spread updates.
 *
 * Why a singleton + in-memory EventEmitter (rather than per-connection DB
 * polling): N connected SSE clients each polling the DB would multiply DB
 * load. With a singleton, the DB is polled once per interval regardless of
 * how many UI clients are connected.
 *
 * Sprint 2.2: DB polling at 500ms. Sprint 2.5+ may switch to
 * Postgres LISTEN/NOTIFY or Redis pub/sub for lower latency.
 */

import { EventEmitter } from 'node:events';
import { getDb } from '@arbitrage-agent/db';
import { createLogger } from '@arbitrage-agent/shared';
import { spreadRowToDto } from './lib/spreadDto.js';

const log = createLogger('api.spreadFeed');

export class SpreadFeed extends EventEmitter {
  private timer: ReturnType<typeof setInterval> | null = null;
  private maxSeenId: bigint = 0n;
  private inFlight = false;

  /**
   * Start polling. First tick initialises maxSeenId from the current DB max
   * so historical rows are not re-emitted as "new" on server restart.
   */
  async start(pollIntervalMs = 500): Promise<void> {
    if (this.timer) return;

    try {
      const newest = await getDb().spread.findFirst({
        orderBy: { id: 'desc' },
        select: { id: true },
      });
      this.maxSeenId = newest?.id ?? 0n;
      log.info({ maxSeenId: this.maxSeenId.toString(), pollIntervalMs }, 'spread feed start');
    } catch (err: unknown) {
      log.warn(
        { err: err instanceof Error ? err.message : String(err) },
        'spread feed init failed — starting from id=0',
      );
    }

    this.timer = setInterval(() => {
      void this.tick();
    }, pollIntervalMs);
  }

  stop(): void {
    if (this.timer) {
      clearInterval(this.timer);
      this.timer = null;
      log.info('spread feed stop');
    }
  }

  /** Override max seen — useful for tests, or for replay. */
  setMaxSeenId(id: bigint): void {
    this.maxSeenId = id;
  }

  private async tick(): Promise<void> {
    if (this.inFlight) return; // skip if previous tick still running (slow DB)
    this.inFlight = true;
    try {
      const rows = await getDb().spread.findMany({
        where: { id: { gt: this.maxSeenId } },
        orderBy: { id: 'asc' },
        take: 100,
      });
      for (const row of rows) {
        if (row.id > this.maxSeenId) this.maxSeenId = row.id;
        this.emit('spread', spreadRowToDto(row));
      }
    } catch (err: unknown) {
      log.warn({ err: err instanceof Error ? err.message : String(err) }, 'tick failed');
    } finally {
      this.inFlight = false;
    }
  }
}

export type { SpreadDto } from './lib/spreadDto.js';
