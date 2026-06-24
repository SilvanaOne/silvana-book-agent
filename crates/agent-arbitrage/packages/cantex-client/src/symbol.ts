import type { Pair } from '@arbitrage-agent/shared';

/**
 * Cantex instrument mapping. Cantex docs write assets as "CC" / "USDCx"
 * (reference-cantex-sdk memory). The real instrument ids come from
 * `get_pool_info()` once onboarded — adjust the table when verified against a
 * live operator key.
 */
const TO_CANTEX: Record<string, string> = {
  CC: 'CC',
  USDCx: 'USDCx',
  CBTC: 'CBTC',
};

export function toCantexInstrument(canonical: string): string {
  return TO_CANTEX[canonical] ?? canonical;
}

/**
 * Cantex launch pools (reference-cantex-sdk memory, 2026-05-26 correction):
 * CC + USDCx only — no multi-stable pairs. CBTC pool is unconfirmed; the real
 * registry comes from `get_pool_info()`. We support CC/USDCx for now.
 */
const SUPPORTED_PAIRS: ReadonlyArray<readonly [string, string]> = [['CC', 'USDCx']];

export function isSupportedPair(pair: Pair): boolean {
  const a = pair.base.symbol;
  const b = pair.quote.symbol;
  return SUPPORTED_PAIRS.some(([x, y]) => (a === x && b === y) || (a === y && b === x));
}
