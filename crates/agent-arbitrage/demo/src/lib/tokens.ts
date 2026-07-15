/**
 * UI-facing token catalogue — CC, CBTC, USDC, CETH, USDCx.
 * Shared by dropdowns, charts, config preview, and demo placeholders.
 */

export const UI_TOKENS = ['CC', 'CBTC', 'USDC', 'CETH', 'USDCx'] as const;
export type UiToken = (typeof UI_TOKENS)[number];

export const TOKEN_COLORS: Record<UiToken, string> = {
  CC: '#FFA752',
  CBTC: '#7C6CF0',
  USDC: '#2775CA',
  CETH: '#2EC4B6',
  USDCx: '#5B9BD5',
};

/** Demo share-of-spreads donut on the Stats tab (placeholder until live data). */
export const DEMO_COIN_SHARE: ReadonlyArray<{ readonly label: UiToken; readonly value: number; readonly color: string }> =
  [
    { label: 'CC', value: 35, color: TOKEN_COLORS.CC },
    { label: 'CBTC', value: 24, color: TOKEN_COLORS.CBTC },
    { label: 'CETH', value: 15, color: TOKEN_COLORS.CETH },
    { label: 'USDCx', value: 16, color: TOKEN_COLORS.USDCx },
    { label: 'USDC', value: 10, color: TOKEN_COLORS.USDC },
  ];

/** Representative trading pairs spanning the UI token set. */
export const UI_TOKEN_PAIRS = [
  { base: 'CC', quote: 'USDT', cluster: 'CEX' },
  { base: 'CC', quote: 'USDCx', cluster: 'Canton · USDCx' },
  { base: 'CC', quote: 'USDC', cluster: 'Canton · USDC' },
  { base: 'CC', quote: 'CBTC', cluster: 'Canton · CBTC' },
  { base: 'CC', quote: 'CETH', cluster: 'Canton · ETH' },
  { base: 'CBTC', quote: 'USDCx', cluster: 'Canton · USDCx' },
  { base: 'CBTC', quote: 'USDC', cluster: 'Canton · USDC' },
  { base: 'CETH', quote: 'USDC', cluster: 'Canton · ETH' },
] as const;

/**
 * Fixed pair list for the dashboard spread-chart dropdown.
 * Never derived from the live SSE feed — mock scanner output omits CBTC/CETH pairs.
 */
export const CHART_PAIR_KEYS = [
  'CC/USDC',
  'CC/USDCx',
  'CC/CBTC',
  'CC/CETH',
  'CBTC/USDCx',
  'CBTC/USDC',
  'CETH/USDC',
] as const;
