// Self-contained demo data ("болванки") for the agent-arbitrage demo.
//
// Ported from crates/agent-arbitrage/apps/web/src/lib/{demo,tokens}.ts and made
// fully standalone: the API/client types (SpreadDto, Profitability) are inlined
// below instead of imported from ./api, and nothing here touches the network.
//
// These synthetic figures seed a believable cross-venue spread dashboard while
// the in-memory simulation accumulates live scans on top of them.

// ── Inlined local types (were imported from ./api) ────────────────────────────

export interface SpreadDto {
  readonly id: string;
  readonly ts: string; // ISO 8601
  readonly basePairKey: string; // e.g. "CC/USDC"
  readonly buyVenueId: string;
  readonly sellVenueId: string;
  readonly buyPrice: string;
  readonly sellPrice: string;
  readonly spreadBps: number;
  readonly estProfitUsd: string;
  readonly acted: boolean;
}

export interface Profitability {
  readonly totals: {
    readonly count: number;
    readonly estProfitUsd: string;
    readonly avgBps: number;
    readonly maxBps: number;
  };
  readonly byVenue: ReadonlyArray<{
    readonly venueId: string;
    readonly count: number;
    readonly buyCount: number;
    readonly sellCount: number;
    readonly estProfitUsd: string;
  }>;
  readonly bySize: ReadonlyArray<{
    readonly label: string;
    readonly min: number;
    readonly max: number | null;
    readonly count: number;
  }>;
}

// ── Token catalogue (from tokens.ts) ─────────────────────────────────────────

export const UI_TOKENS = ["CC", "CBTC", "USDC", "CETH", "USDCx"] as const;
export type UiToken = (typeof UI_TOKENS)[number];

export const TOKEN_COLORS: Record<UiToken, string> = {
  CC: "#FFA752",
  CBTC: "#7C6CF0",
  USDC: "#2775CA",
  CETH: "#2EC4B6",
  USDCx: "#5B9BD5",
};

/** Fixed pair list for the dashboard spread-chart dropdown. */
export const CHART_PAIR_KEYS = [
  "CC/USDC",
  "CC/USDCx",
  "CC/CBTC",
  "CC/CETH",
  "CBTC/USDCx",
  "CBTC/USDC",
  "CETH/USDC",
] as const;

export type ChartPairKey = (typeof CHART_PAIR_KEYS)[number];

// ── Venue catalogue ──────────────────────────────────────────────────────────

export interface VenueMeta {
  readonly name: string;
  readonly kind: "canton" | "cex";
  readonly color: string;
}

export const VENUES: Record<string, VenueMeta> = {
  silvana: { name: "Silvana", kind: "canton", color: "#ff8a1c" },
  cantex: { name: "Cantex", kind: "canton", color: "#5b8def" },
  oneswap: { name: "OneSwap", kind: "canton", color: "#8b5cf6" },
  temple: { name: "Temple", kind: "canton", color: "#22c55e" },
  bybit: { name: "Bybit", kind: "cex", color: "#f59e0b" },
  kucoin: { name: "KuCoin", kind: "cex", color: "#2ec4b6" },
};

/** Canton venues the scanner routes across for the live simulation. */
export const CANTON_VENUES = ["silvana", "cantex", "oneswap", "temple"] as const;

export function venueName(id: string): string {
  return VENUES[id]?.name ?? id;
}

export function venueColor(id: string): string {
  return VENUES[id]?.color ?? "#8a8a99";
}

// ── Hand-tuned per-pair spread curves (from demo.ts) ─────────────────────────

export interface PairCurve {
  readonly buyVenue: string;
  readonly sellVenue: string;
  readonly buyPrice: string;
  readonly bps: readonly number[];
}

export const CHART_BPS_CURVES: Readonly<Record<ChartPairKey, PairCurve>> = {
  "CC/USDC": {
    buyVenue: "silvana",
    sellVenue: "cantex",
    buyPrice: "0.0212",
    bps: [155, 161, 158, 164, 170, 167, 173, 179, 176, 182, 188, 185, 191, 197, 194, 200, 206, 203, 209, 215],
  },
  "CC/USDCx": {
    buyVenue: "silvana",
    sellVenue: "oneswap",
    buyPrice: "0.0205",
    bps: [185, 191, 188, 194, 200, 197, 203, 209, 206, 212, 218, 215, 221, 227, 224, 230, 236, 233, 239, 245],
  },
  "CC/CBTC": {
    buyVenue: "oneswap",
    sellVenue: "temple",
    buyPrice: "0.00000021",
    bps: [132, 138, 135, 141, 147, 144, 150, 156, 153, 159, 165, 162, 168, 174, 171, 177, 183, 180, 186, 192],
  },
  "CC/CETH": {
    buyVenue: "oneswap",
    sellVenue: "temple",
    buyPrice: "0.0000062",
    bps: [118, 124, 121, 127, 133, 130, 136, 142, 139, 145, 151, 148, 154, 160, 157, 163, 169, 166, 172, 178],
  },
  "CBTC/USDCx": {
    buyVenue: "silvana",
    sellVenue: "temple",
    buyPrice: "99800",
    bps: [245, 251, 248, 254, 260, 257, 263, 269, 266, 272, 278, 275, 281, 287, 284, 290, 296, 293, 299, 305],
  },
  "CBTC/USDC": {
    buyVenue: "silvana",
    sellVenue: "cantex",
    buyPrice: "95680",
    bps: [268, 274, 271, 277, 283, 280, 286, 292, 289, 295, 301, 298, 304, 310, 307, 313, 319, 316, 322, 328],
  },
  "CETH/USDC": {
    buyVenue: "silvana",
    sellVenue: "temple",
    buyPrice: "3420",
    bps: [128, 134, 131, 137, 143, 140, 146, 152, 149, 155, 161, 158, 164, 170, 167, 173, 179, 176, 182, 188],
  },
};

/** Mean bps per pair — the mean-reverting anchor for the live simulation. */
export const BASE_BPS: Readonly<Record<ChartPairKey, number>> = (() => {
  const out = {} as Record<ChartPairKey, number>;
  for (const pair of CHART_PAIR_KEYS) {
    const arr = CHART_BPS_CURVES[pair].bps;
    out[pair] = Math.round(arr.reduce((s, v) => s + v, 0) / arr.length);
  }
  return out;
})();

export function decimalsFor(buyPrice: number): number {
  return buyPrice >= 1 ? 2 : buyPrice >= 0.01 ? 8 : 12;
}

// ── Demo generators (ported) ─────────────────────────────────────────────────

/**
 * Fixed demo spread time-series (~20 ticks, 30 s apart) covering every pair.
 * Ordered ascending by ts.
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
        sellPrice: sellPrice.toFixed(decimalsFor(buyPrice)),
        spreadBps: bps,
        estProfitUsd: (bps / 100).toFixed(4),
        acted: false,
      });
    }
  }
  return out;
}

/** Supplemental demo rows for the "Recent spreads" table (multi-venue). */
export function demoSupplementalSpreads(now: number = Date.now()): SpreadDto[] {
  const rows: ReadonlyArray<readonly [string, string, string, string, number]> = [
    // basePairKey, buyVenue, sellVenue, buyPrice, spreadBps
    ["CBTC/USDCx", "temple", "silvana", "95400", 312],
    ["CC/CBTC", "oneswap", "cantex", "0.00000021", 188],
    ["CC/USDCx", "cantex", "temple", "0.0201", 245],
    ["CETH/USDC", "temple", "silvana", "3418", 228],
    ["CBTC/USDC", "silvana", "cantex", "95680", 410],
    ["CC/CETH", "oneswap", "temple", "0.0000062", 167],
    ["CBTC/USDCx", "temple", "oneswap", "95350", 295],
    ["CC/USDCx", "oneswap", "cantex", "0.0199", 178],
    ["CBTC/USDC", "silvana", "temple", "95590", 358],
    ["CC/CBTC", "silvana", "oneswap", "0.00000022", 142],
    ["CETH/USDC", "silvana", "cantex", "3412", 271],
    ["CC/USDCx", "temple", "silvana", "0.0203", 312],
    ["CC/CBTC", "cantex", "temple", "0.00000019", 134],
    ["CBTC/USDCx", "cantex", "silvana", "95220", 326],
    ["CBTC/USDC", "temple", "silvana", "95720", 388],
    ["CC/CETH", "oneswap", "silvana", "0.0000061", 218],
    ["CETH/USDC", "temple", "oneswap", "3420", 196],
    ["CC/USDCx", "silvana", "oneswap", "0.0206", 205],
    ["CBTC/USDC", "cantex", "silvana", "95180", 298],
    ["CC/CETH", "cantex", "silvana", "0.0000063", 173],
    ["CETH/USDC", "silvana", "temple", "3430", 281],
    ["CBTC/USDCx", "temple", "cantex", "95440", 257],
    ["CC/CBTC", "oneswap", "temple", "0.00000020", 156],
    ["CC/USDCx", "cantex", "oneswap", "0.0198", 189],
    ["CETH/USDC", "silvana", "oneswap", "3415", 365],
    ["CC/CETH", "temple", "cantex", "0.0000060", 124],
    ["CBTC/USDC", "silvana", "cantex", "95850", 462],
    ["CBTC/USDCx", "oneswap", "silvana", "95460", 303],
    ["CC/CBTC", "temple", "silvana", "0.00000022", 211],
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

/** Demo profitability snapshot — totals, per-venue participation, histogram. */
export function demoProfitability(): Profitability {
  return {
    totals: {
      count: 42,
      estProfitUsd: "3.18",
      avgBps: 185,
      maxBps: 410,
    },
    byVenue: [
      { venueId: "silvana", count: 15, buyCount: 8, sellCount: 7, estProfitUsd: "1.20" },
      { venueId: "cantex", count: 12, buyCount: 7, sellCount: 5, estProfitUsd: "0.92" },
      { venueId: "oneswap", count: 10, buyCount: 5, sellCount: 5, estProfitUsd: "0.55" },
      { venueId: "temple", count: 8, buyCount: 4, sellCount: 4, estProfitUsd: "0.30" },
      { venueId: "bybit", count: 4, buyCount: 3, sellCount: 1, estProfitUsd: "0.12" },
      { venueId: "kucoin", count: 3, buyCount: 1, sellCount: 2, estProfitUsd: "0.09" },
    ],
    bySize: [
      { label: "<50", min: 0, max: 50, count: 3 },
      { label: "50–100", min: 50, max: 100, count: 8 },
      { label: "100–200", min: 100, max: 200, count: 14 },
      { label: "200–300", min: 200, max: 300, count: 9 },
      { label: "300–500", min: 300, max: 500, count: 6 },
      { label: "500+", min: 500, max: null, count: 2 },
    ],
  };
}
