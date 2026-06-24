import type { Pair } from '@arbitrage-agent/shared';

/**
 * OneSwap symbol mapping. The SDK/API uses Canton-native asset names that
 * differ from our canonical symbols:
 *   CC    → Amulet   (Canton Coin's registry name)
 *   CBTC  → cBTC
 *   USDCx → USDCx    (unchanged)
 *
 * Per md_docs reference + reference-oneswap-sdk memory.
 */
const TO_ONESWAP: Record<string, string> = {
  CC: 'Amulet',
  CBTC: 'cBTC',
  USDCx: 'USDCx',
};

export function toOneSwapSymbol(canonical: string): string {
  return TO_ONESWAP[canonical] ?? canonical;
}

/**
 * Confirmed OneSwap pools (reference-oneswap-sdk memory):
 *   Amulet/USDCx (= CC/USDCx)
 *   Amulet/cBTC  (= CC/CBTC)
 * We match on canonical base/quote symbols regardless of order.
 */
const SUPPORTED_PAIRS: ReadonlyArray<readonly [string, string]> = [
  ['CC', 'USDCx'],
  ['CC', 'CBTC'],
];

export function isSupportedPair(pair: Pair): boolean {
  const a = pair.base.symbol;
  const b = pair.quote.symbol;
  return SUPPORTED_PAIRS.some(
    ([x, y]) => (a === x && b === y) || (a === y && b === x),
  );
}
