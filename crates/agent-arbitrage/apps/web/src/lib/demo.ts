/**
 * Demo data ("болванки") for charts/diagrams while the agent isn't yet wired
 * to on-chain transactions and real exchange APIs.
 *
 * Used by the dashboard spread chart and the Stats tab whenever real spreads
 * haven't accumulated yet — the UI substitutes these synthetic figures and
 * surfaces a "DEMO" badge so the operator knows it isn't live data.
 *
 * Token set for the dummies: CC, CBTC, USDC, CETH, USDCx.
 */

import type { SpreadDto, Profitability } from './api';
import { UI_TOKENS, UI_TOKEN_PAIRS, CHART_PAIR_KEYS } from './tokens';

export { CHART_PAIR_KEYS };

export const DEMO_TOKENS = UI_TOKENS;

/** A handful of representative trading pairs spanning the demo token set. */
export const DEMO_PAIRS = UI_TOKEN_PAIRS.filter((p) => p.quote !== 'USDT').map((p) => ({
  base: p.base,
  quote: p.quote,
}));

/**
 * Hand-tuned spread bps curves for the dashboard chart (20 ticks each).
 *
 * Smooth, market-like drift — no alternating spikes (which produced the
 * sawtooth artefact when live scanner ticks were plotted). Values are fixed;
 * do not derive from runtime data.
 */
const CHART_BPS_CURVES: Readonly<
  Record<
    (typeof CHART_PAIR_KEYS)[number],
    { readonly buyVenue: string; readonly sellVenue: string; readonly buyPrice: string; readonly bps: readonly number[] }
  >
> = {
  'CC/USDC': {
    buyVenue: 'silvana',
    sellVenue: 'cantex',
    buyPrice: '0.0212',
    bps: [155, 161, 158, 164, 170, 167, 173, 179, 176, 182, 188, 185, 191, 197, 194, 200, 206, 203, 209, 215],
  },
  'CC/USDCx': {
    buyVenue: 'silvana',
    sellVenue: 'oneswap',
    buyPrice: '0.0205',
    bps: [185, 191, 188, 194, 200, 197, 203, 209, 206, 212, 218, 215, 221, 227, 224, 230, 236, 233, 239, 245],
  },
  'CC/CBTC': {
    buyVenue: 'oneswap',
    sellVenue: 'temple',
    buyPrice: '0.00000021',
    bps: [132, 138, 135, 141, 147, 144, 150, 156, 153, 159, 165, 162, 168, 174, 171, 177, 183, 180, 186, 192],
  },
  'CC/CETH': {
    buyVenue: 'oneswap',
    sellVenue: 'temple',
    buyPrice: '0.0000062',
    bps: [118, 124, 121, 127, 133, 130, 136, 142, 139, 145, 151, 148, 154, 160, 157, 163, 169, 166, 172, 178],
  },
  'CBTC/USDCx': {
    buyVenue: 'silvana',
    sellVenue: 'temple',
    buyPrice: '99800',
    bps: [245, 251, 248, 254, 260, 257, 263, 269, 266, 272, 278, 275, 281, 287, 284, 290, 296, 293, 299, 305],
  },
  'CBTC/USDC': {
    buyVenue: 'silvana',
    sellVenue: 'cantex',
    buyPrice: '95680',
    bps: [268, 274, 271, 277, 283, 280, 286, 292, 289, 295, 301, 298, 304, 310, 307, 313, 319, 316, 322, 328],
  },
  'CETH/USDC': {
    buyVenue: 'silvana',
    sellVenue: 'temple',
    buyPrice: '3420',
    bps: [128, 134, 131, 137, 143, 140, 146, 152, 149, 155, 161, 158, 164, 170, 167, 173, 179, 176, 182, 188],
  },
};

/**
 * Fixed demo spread time-series for the dashboard chart (~20 ticks, 30 s apart).
 * Covers every entry in CHART_PAIR_KEYS. Ordered ascending by ts.
 */
export function demoSpreads(now: number = Date.now()): SpreadDto[] {
  const out: SpreadDto[] = [];
  let id = 1;
  for (const pairKey of CHART_PAIR_KEYS) {
    const spec = CHART_BPS_CURVES[pairKey];
    for (let i = 0; i < spec.bps.length; i++) {
      const bps = spec.bps[i]!;
      const ts = new Date(now - (spec.bps.length - i) * 30_000).toISOString();
      const buyPrice = Number(spec.buyPrice);
      const sellPrice = buyPrice * (1 + bps / 10_000);
      out.push({
        id: `demo-${id++}`,
        ts,
        basePairKey: pairKey,
        buyVenueId: spec.buyVenue,
        sellVenueId: spec.sellVenue,
        buyPrice: spec.buyPrice,
        sellPrice: sellPrice.toFixed(buyPrice >= 1 ? 2 : buyPrice >= 0.01 ? 8 : 12),
        spreadBps: bps,
        estProfitUsd: (bps / 100).toFixed(4),
        acted: false,
      });
    }
  }
  return out;
}

/**
 * Supplemental demo spread rows for the dashboard "Recent spreads" table.
 *
 * In mock mode the scanner doesn't yet produce CBTC/CETH/USDCx cross-venue spreads
 * across all Canton venues, so without these we'd see gaps in the token
 * catalogue. Returns realistic-looking entries spread across multiple venues;
 * ids are prefixed `demo-sup-` so the table can flag them as synthetic.
 */
export function demoSupplementalSpreads(now: number = Date.now()): SpreadDto[] {
  const rows: ReadonlyArray<readonly [string, string, string, string, number]> = [
    // basePairKey, buyVenue, sellVenue, buyPrice, spreadBps
    ['CBTC/USDCx', 'temple', 'silvana', '95400', 312],
    ['CC/CBTC', 'oneswap', 'cantex', '0.00000021', 188],
    ['CC/USDCx', 'cantex', 'temple', '0.0201', 245],
    ['CETH/USDC', 'temple', 'silvana', '3418', 228],
    ['CBTC/USDC', 'silvana', 'cantex', '95680', 410],
    ['CC/CETH', 'oneswap', 'temple', '0.0000062', 167],
    ['CBTC/USDCx', 'temple', 'oneswap', '95350', 295],
    ['CC/USDCx', 'oneswap', 'cantex', '0.0199', 178],
    ['CBTC/USDC', 'silvana', 'temple', '95590', 358],
    ['CC/CBTC', 'silvana', 'oneswap', '0.00000022', 142],
    // ── second wave ───────────────────────────────────────────────────────
    ['CETH/USDC', 'silvana', 'cantex', '3412', 271],
    ['CC/USDCx', 'temple', 'silvana', '0.0203', 312],
    ['CC/CBTC', 'cantex', 'temple', '0.00000019', 134],
    ['CBTC/USDCx', 'cantex', 'silvana', '95220', 326],
    ['CBTC/USDC', 'temple', 'silvana', '95720', 388],
    ['CC/CETH', 'oneswap', 'silvana', '0.0000061', 218],
    ['CETH/USDC', 'temple', 'oneswap', '3420', 196],
    ['CC/USDCx', 'silvana', 'oneswap', '0.0206', 205],
    ['CBTC/USDC', 'cantex', 'silvana', '95180', 298],
    ['CC/CETH', 'cantex', 'silvana', '0.0000063', 173],
    // ── third wave ────────────────────────────────────────────────────────
    ['CETH/USDC', 'silvana', 'temple', '3430', 281],
    ['CBTC/USDCx', 'temple', 'cantex', '95440', 257],
    ['CC/CBTC', 'oneswap', 'temple', '0.00000020', 156],
    ['CC/USDCx', 'cantex', 'oneswap', '0.0198', 189],
    ['CETH/USDC', 'silvana', 'oneswap', '3415', 365],
    ['CC/CETH', 'temple', 'cantex', '0.0000060', 124],
    ['CBTC/USDC', 'silvana', 'cantex', '95850', 462],
    ['CBTC/USDCx', 'oneswap', 'silvana', '95460', 303],
    ['CC/CBTC', 'temple', 'silvana', '0.00000022', 211],
  ];
  return rows.map(([pairKey, buy, sell, buyPriceStr, bps], i) => {
    const buyPrice = Number(buyPriceStr);
    const sellPrice = buyPrice * (1 + bps / 10_000);
    const ts = new Date(now - (i + 1) * 45_000).toISOString();
    return {
      id: `demo-sup-${i + 1}`,
      ts,
      basePairKey: pairKey,
      buyVenueId: buy,
      sellVenueId: sell,
      buyPrice: buyPriceStr,
      sellPrice: sellPrice.toFixed(buyPrice >= 1 ? 2 : 12),
      spreadBps: bps,
      estProfitUsd: (bps / 100).toFixed(4),
      acted: false,
    };
  });
}

/** @deprecated use demoSupplementalSpreads */
export const demoCBTCSpreads = demoSupplementalSpreads;

/**
 * Demo profitability snapshot — totals, per-venue participation (Silvana FIRST),
 * and a believable spread-size histogram across the demo token set.
 */
export function demoProfitability(): Profitability {
  return {
    totals: {
      count: 42,
      estProfitUsd: '3.18',
      avgBps: 185,
      maxBps: 410,
    },
    byVenue: [
      { venueId: 'silvana', count: 15, buyCount: 8, sellCount: 7, estProfitUsd: '1.20' },
      { venueId: 'cantex', count: 12, buyCount: 7, sellCount: 5, estProfitUsd: '0.92' },
      { venueId: 'oneswap', count: 10, buyCount: 5, sellCount: 5, estProfitUsd: '0.55' },
      { venueId: 'temple', count: 8, buyCount: 4, sellCount: 4, estProfitUsd: '0.30' },
      { venueId: 'bybit', count: 4, buyCount: 3, sellCount: 1, estProfitUsd: '0.12' },
      { venueId: 'kucoin', count: 3, buyCount: 1, sellCount: 2, estProfitUsd: '0.09' },
    ],
    bySize: [
      { label: '<50', min: 0, max: 50, count: 3 },
      { label: '50–100', min: 50, max: 100, count: 8 },
      { label: '100–200', min: 100, max: 200, count: 14 },
      { label: '200–300', min: 200, max: 300, count: 9 },
      { label: '300–500', min: 300, max: 500, count: 6 },
      { label: '500+', min: 500, max: null, count: 2 },
    ],
  };
}
