// Self-contained port of the target-weight rebalancing math used by
// agent-batch-order-management. Mirrors the shapes/labels in
//   crates/agent-batch-order-management/packages/portfolio-engine/src/rebalance.ts
//   crates/agent-batch-order-management/apps/api/src/routes/portfolio.ts
// but with plain-number math (no Decimal, no Prisma) so the demo is standalone.
//
// Fixtures are the exact numbers from prisma/seed.ts (the "Development portfolio").
//   NAV ≈ 20406 USDC.

export const PORTFOLIO_ID = "00000000-0000-4000-a000-000000000001";
export const PORTFOLIO_NAME = "Development portfolio";
export const QUOTE_CURRENCY = "USDC";
export const DEFAULT_THRESHOLD_BPS = 60;

export type Position = { assetSymbol: string; qty: number; price: number };

export type Target = {
  assetSymbol: string;
  targetWeight: number; // 0..1
  minWeight: number | null;
  maxWeight: number | null;
  enabled: boolean;
};

// --- exact seed fixtures (prisma/seed.ts) ---------------------------------

export const SEED_TARGETS: readonly Target[] = [
  { assetSymbol: "WBTC", targetWeight: 0.25, minWeight: null, maxWeight: null, enabled: true },
  { assetSymbol: "WETH", targetWeight: 0.25, minWeight: null, maxWeight: null, enabled: true },
  { assetSymbol: "CC", targetWeight: 0.1, minWeight: null, maxWeight: null, enabled: true },
  { assetSymbol: "USDC", targetWeight: 0.4, minWeight: 0.1, maxWeight: 0.9, enabled: true },
];

export const SEED_POSITIONS: readonly Position[] = [
  { assetSymbol: "WBTC", qty: 0.05, price: 100000 },
  { assetSymbol: "WETH", qty: 1.5, price: 3500 },
  { assetSymbol: "CC", qty: 1000, price: 0.156 },
  { assetSymbol: "USDC", qty: 10000, price: 1 },
];

// Execution venues the batch job fans out across (display only).
export const VENUES: readonly { id: string; label: string; role: string }[] = [
  { id: "silvana-canton", label: "Silvana Canton DEX", role: "primary" },
  { id: "rfq-pool", label: "RFQ liquidity pool", role: "backup" },
];

// --- types ----------------------------------------------------------------

export type DriftRow = {
  assetSymbol: string;
  qty: number;
  price: number;
  value: number;
  currentWeight: number;
  targetWeight: number;
  driftBps: number; // (target − current) × 10000
  inBand: boolean; // |driftBps| <= thresholdBps
  isQuote: boolean;
};

export type PlannedOrder = {
  assetSymbol: string;
  market: string; // ASSET-QUOTE
  side: "buy" | "sell";
  qty: number;
  price: number;
  notional: number;
  venueId: string;
};

export type Transfer = {
  from: string;
  to: string;
  amountFrom: number;
  amountTo: number;
  txHash: string;
  venueId: string;
  note: string;
};

export type DriftAnalysis = {
  quoteCurrency: string;
  nav: number;
  thresholdBps: number;
  rows: DriftRow[];
  maxAbsDriftBps: number;
  breaches: number;
};

export function marketValue(p: Position): number {
  return p.qty * p.price;
}

export function computeNav(positions: readonly Position[]): number {
  return positions.reduce((s, p) => s + marketValue(p), 0);
}

/** Always-on drift table: NAV, per-asset current vs target weight, drift bps, in-band flag. */
export function analyzeDrift(
  positions: readonly Position[],
  targets: readonly Target[],
  thresholdBps: number,
  quote = QUOTE_CURRENCY,
): DriftAnalysis {
  const enabled = targets.filter((t) => t.enabled);
  const posBySym = new Map(positions.map((p) => [p.assetSymbol, p] as const));
  const nav = enabled.reduce((s, t) => {
    const p = posBySym.get(t.assetSymbol);
    return s + (p ? marketValue(p) : 0);
  }, 0);

  const rows: DriftRow[] = enabled.map((t) => {
    const p = posBySym.get(t.assetSymbol);
    const value = p ? marketValue(p) : 0;
    const currentWeight = nav > 0 ? value / nav : 0;
    const driftBps = (t.targetWeight - currentWeight) * 10000;
    return {
      assetSymbol: t.assetSymbol,
      qty: p?.qty ?? 0,
      price: p?.price ?? 0,
      value,
      currentWeight,
      targetWeight: t.targetWeight,
      driftBps,
      inBand: Math.abs(driftBps) <= thresholdBps,
      isQuote: t.assetSymbol === quote,
    };
  });

  const maxAbsDriftBps = rows.reduce((m, r) => Math.max(m, Math.abs(r.driftBps)), 0);
  const breaches = rows.filter((r) => !r.inBand).length;
  return { quoteCurrency: quote, nav, thresholdBps, rows, maxAbsDriftBps, breaches };
}

/**
 * Plan limit orders for every breached, non-quote asset.
 *   notional = |target − current| × NAV
 *   side     = buy when under-weight (target > current), sell when over-weight
 *   qty      = notional / price
 * The quote currency (USDC) is the implicit funding leg and never gets its own order.
 */
export function planOrders(analysis: DriftAnalysis): PlannedOrder[] {
  const orders: PlannedOrder[] = [];
  let vi = 0;
  for (const r of analysis.rows) {
    if (r.isQuote || r.inBand || r.price <= 0) continue;
    const notional = Math.abs(r.driftBps / 10000) * analysis.nav;
    if (notional <= 0) continue;
    const side: "buy" | "sell" = r.driftBps > 0 ? "buy" : "sell";
    const qty = notional / r.price;
    const venueId = VENUES[vi % VENUES.length].id;
    vi += 1;
    orders.push({
      assetSymbol: r.assetSymbol,
      market: `${r.assetSymbol}-${analysis.quoteCurrency}`,
      side,
      qty: round8(qty),
      price: r.price,
      notional: round2(notional),
      venueId,
    });
  }
  return orders;
}

/** Pair each planned order with a settlement transfer + mock Canton tx hash. */
export function planTransfers(orders: readonly PlannedOrder[], quote = QUOTE_CURRENCY): Transfer[] {
  return orders.map((o) => {
    if (o.side === "buy") {
      return {
        from: quote,
        to: o.assetSymbol,
        amountFrom: o.notional,
        amountTo: o.qty,
        txHash: mockTxHash(),
        venueId: o.venueId,
        note: `Convert ${o.notional.toFixed(2)} ${quote} into ${o.qty.toFixed(4)} ${o.assetSymbol} @ ${o.price}`,
      };
    }
    return {
      from: o.assetSymbol,
      to: quote,
      amountFrom: o.qty,
      amountTo: o.notional,
      txHash: mockTxHash(),
      venueId: o.venueId,
      note: `Convert ${o.qty.toFixed(4)} ${o.assetSymbol} into ${o.notional.toFixed(2)} ${quote} @ ${o.price}`,
    };
  });
}

/**
 * Target-aligned snapshot applied when a rebalance job completes: every enabled
 * target ends holding exactly weight × NAV (drift → ~0). Prices are preserved.
 */
export function targetAlignedPositions(
  positions: readonly Position[],
  targets: readonly Target[],
  quote = QUOTE_CURRENCY,
): Position[] {
  const analysis = analyzeDrift(positions, targets, 0, quote);
  const nav = analysis.nav;
  const posBySym = new Map(positions.map((p) => [p.assetSymbol, p] as const));
  const enabled = targets.filter((t) => t.enabled);
  const totalWeight = enabled.reduce((s, t) => s + t.targetWeight, 0) || 1;
  return enabled.map((t) => {
    const price = posBySym.get(t.assetSymbol)?.price ?? 1;
    const mv = (t.targetWeight / totalWeight) * nav;
    return { assetSymbol: t.assetSymbol, price, qty: price > 0 ? round8(mv / price) : 0 };
  });
}

export function round2(n: number): number {
  return Math.round(n * 100) / 100;
}
export function round8(n: number): number {
  return Math.round(n * 1e8) / 1e8;
}

const HEX = "0123456789abcdef";
export function mockTxHash(): string {
  let s = "0x";
  for (let i = 0; i < 64; i++) s += HEX[(Math.random() * 16) | 0];
  return s;
}

export function shortHash(h: string): string {
  return h.length > 14 ? `${h.slice(0, 8)}…${h.slice(-4)}` : h;
}
