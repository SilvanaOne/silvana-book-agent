import type { Pair } from '@arbitrage-agent/shared';

/**
 * Temple symbol mapping. Temple writes pairs with canonical asset tickers
 * (CC/USDCx, CBTC/USDCx) — unlike OneSwap which uses "Amulet" for CC. We keep
 * an explicit table so the live REST path can be adjusted if the venue's
 * symbol notation differs once we have an API key to verify against.
 */
const TO_TEMPLE: Record<string, string> = {
  CC: 'CC',
  CBTC: 'CBTC',
  USDCx: 'USDCx',
};

export function toTempleAsset(canonical: string): string {
  return TO_TEMPLE[canonical] ?? canonical;
}

/** Temple market symbol, e.g. 'CC/USDCx'. */
export function toTempleSymbol(pair: Pair): string {
  return `${toTempleAsset(pair.base.symbol)}/${toTempleAsset(pair.quote.symbol)}`;
}

/**
 * Temple-listed pairs (reference-temple-sdk memory): CC/USDCx, CBTC/USDCx.
 * USDCx is the primary quote. We match on canonical base/quote regardless of
 * order so a flipped Pair still resolves.
 */
const SUPPORTED_PAIRS: ReadonlyArray<readonly [string, string]> = [
  ['CC', 'USDCx'],
  ['CBTC', 'USDCx'],
];

export function isSupportedPair(pair: Pair): boolean {
  const a = pair.base.symbol;
  const b = pair.quote.symbol;
  return SUPPORTED_PAIRS.some(([x, y]) => (a === x && b === y) || (a === y && b === x));
}
