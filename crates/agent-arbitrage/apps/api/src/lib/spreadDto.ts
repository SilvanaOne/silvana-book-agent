import type { Spread } from '@arbitrage-agent/db';

export interface SpreadDto {
  readonly id: string;
  readonly ts: string;
  readonly basePairKey: string;
  readonly buyVenueId: string;
  readonly sellVenueId: string;
  readonly buyPrice: string;
  readonly sellPrice: string;
  readonly spreadBps: number;
  readonly estProfitUsd: string;
  readonly acted: boolean;
}

export function spreadRowToDto(row: Spread): SpreadDto {
  return {
    id: row.id.toString(),
    ts: row.ts.toISOString(),
    basePairKey: row.basePairKey,
    buyVenueId: row.buyVenueId,
    sellVenueId: row.sellVenueId,
    buyPrice: row.buyPrice.toString(),
    sellPrice: row.sellPrice.toString(),
    spreadBps: row.spreadBps,
    estProfitUsd: row.estProfitUsd.toString(),
    acted: row.acted,
  };
}
