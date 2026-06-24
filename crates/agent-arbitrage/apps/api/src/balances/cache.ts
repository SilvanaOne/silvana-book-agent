import type { BalanceSnapshot } from '@arbitrage-agent/clients';

const EMPTY: BalanceSnapshot = {
  updatedAt: 0,
  totalUsd: 0,
  sources: [],
  rows: [],
};

let cached: BalanceSnapshot = EMPTY;

export function getCachedBalances(): BalanceSnapshot {
  return cached;
}

export function setCachedBalances(snap: BalanceSnapshot): void {
  cached = snap;
}

export { EMPTY as EMPTY_BALANCE_SNAPSHOT };
