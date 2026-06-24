import type { Pair } from '@arbitrage-agent/shared';

/**
 * Silvana Book market id format is hyphenated, e.g. `CC-USDC`
 * (reference-silvana-book memory: `cloud-agent buy --market CC-USDC`).
 *
 * NOTE: Silvana's quote stable is **USDC** (a Splice/testnet USDC), distinct
 * from the USDCx used by the Temple/OneSwap/Cantex cluster. The two only
 * arbitrage against each other via CC-triangulation + a stablecoin conversion
 * cost — that cross-cluster detection lands in Sprint 7.
 */
export function toSilvanaMarket(pair: Pair): string {
  return `${pair.base.symbol}-${pair.quote.symbol}`;
}

/** Silvana Book confirmed market: CC/USDC (CC-USDC). */
const SUPPORTED_PAIRS: ReadonlyArray<readonly [string, string]> = [['CC', 'USDC']];

export function isSupportedPair(pair: Pair): boolean {
  const a = pair.base.symbol;
  const b = pair.quote.symbol;
  return SUPPORTED_PAIRS.some(([x, y]) => (a === x && b === y) || (a === y && b === x));
}
