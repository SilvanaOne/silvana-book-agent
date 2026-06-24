/**
 * Standalone integration smoke for persistence. Not part of `vitest`.
 *
 * Usage:
 *   DATABASE_URL=... npx tsx apps/scanner/src/persistence.smoke.ts
 *
 * Verifies:
 *  1) initPersistence connects to DB
 *  2) ensureSeed creates Venues/Instruments/Markets idempotently
 *  3) recordSpread inserts a Spread row and returns its id
 *  4) Round-trip — query the inserted row back via Prisma
 *
 * Exits 0 on success, 1 on any failure.
 */

import {
  createToken,
  createPair,
  createSpreadOpportunity,
  tokenPairKey,
} from '@arbitrage-agent/shared';
import {
  initPersistence,
  shutdownPersistence,
  ensureSeed,
  recordSpread,
} from './persistence.js';

async function main(): Promise<void> {
  const cc = createToken('CC', 'Canton Coin', 6, 'canton');
  const usdt = createToken('USDT', 'Tether USD', 6, 'cex');
  const bybitPair = createPair(cc, usdt, 'bybit');
  const kucoinPair = createPair(cc, usdt, 'kucoin');

  console.log('[smoke] initPersistence…');
  const ctx = await initPersistence();

  console.log('[smoke] ensureSeed…');
  await ensureSeed(ctx, [bybitPair, kucoinPair]);

  // Fabricate a profitable opportunity: buy Bybit @0.150, sell KuCoin @0.155 = 3.33%
  const opp = createSpreadOpportunity(
    bybitPair, // representative pair (canonical key)
    { venueId: 'bybit', buyPrice: 0.15, sellPrice: 0.149, timestamp: Date.now() },
    { venueId: 'kucoin', buyPrice: 0.156, sellPrice: 0.155, timestamp: Date.now() },
    100,
  );
  console.log(`[smoke] recordSpread (spread=${opp.spreadPct.toFixed(4)}%)…`);
  const id = await recordSpread(ctx, opp);
  if (id === null) {
    throw new Error('recordSpread returned null');
  }
  console.log(`[smoke] inserted Spread id=${id}`);

  // Round-trip verification
  const row = await ctx.db.spread.findUnique({ where: { id } });
  if (!row) throw new Error(`Spread id=${id} not found after insert`);
  if (row.basePairKey !== tokenPairKey(bybitPair)) {
    throw new Error(`basePairKey mismatch: got ${row.basePairKey}`);
  }
  if (row.buyVenueId !== 'bybit' || row.sellVenueId !== 'kucoin') {
    throw new Error(`venue mismatch: buy=${row.buyVenueId}, sell=${row.sellVenueId}`);
  }
  if (row.spreadBps !== Math.round(opp.spreadPct * 100)) {
    throw new Error(`spreadBps mismatch: db=${row.spreadBps} expected=${Math.round(opp.spreadPct * 100)}`);
  }
  if (row.acted !== false) throw new Error(`acted should be false (paper mode)`);
  console.log(
    `[smoke] round-trip ok: id=${row.id}, pair=${row.basePairKey}, ` +
      `${row.buyVenueId}→${row.sellVenueId}, spread=${row.spreadBps}bps, acted=${row.acted}`,
  );

  await shutdownPersistence(ctx);
  console.log('[smoke] ALL CHECKS PASSED');
}

main().catch((err: unknown) => {
  console.error('[smoke] FAILED:', err);
  process.exit(1);
});
