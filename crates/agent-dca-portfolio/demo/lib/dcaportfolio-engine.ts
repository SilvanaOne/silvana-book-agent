// Port of agent-dca-portfolio placement logic to TypeScript. Mirrors
// crates/agent-dca-portfolio/src/main.rs (fn dca_loop, fanned out across
// every market in --markets).
//
// Rule (per-market DCA):
//   every `intervalSecs` seconds, place one limit order on `market`.
//   price = mid × (1 + priceOffsetPct/100), where mid is the current
//   market mid at that tick. The demo runs an independent GBM walk per
//   market, so mid drifts on its own each tick.
//
// The step() signature receives the current wall-clock so the store can
// drive all markets on the same tick. It ignores any externally supplied
// price — each market advances its own internal mid.
//
// A market stops issuing orders once totalQty reaches `maxTotal` (if set).
// When every market is complete, overall status flips to "completed".
//
// This is a UI teaching model, not a backtest.

export type Side = "buy" | "sell";
export type OrderType = "BID" | "OFFER";

export type DcaOrder = Readonly<{
  seq: number;      // global order sequence across all markets
  t: number;        // epoch ms of placement
  market: string;
  side: Side;
  type: OrderType;
  price: number;    // rounded to 8 decimals
  qty: number;
  notional: number; // price * qty
  mid: number;      // observed mid at placement time
}>;

export type DcaPortfolioConfig = Readonly<{
  markets: string[];       // e.g. ["CBTC-CC","CETH-CC"]
  side: Side;
  amountPerOrder: number;  // qty per order per market
  intervalSecs: number;    // seconds between consecutive orders on the same market
  priceOffsetPct: number;  // e.g. -0.5 = 0.5% below mid for buys
  maxTotal: number | null; // stop a market once totalQty >= maxTotal
  startingPrices: number[]; // parallel to markets
}>;

export type MarketProgress = {
  market: string;
  currentPrice: number;    // internal GBM mid for this market
  lastOrderAt: number | null; // epoch ms of last placement, null before first
  totalQty: number;
  totalNotional: number;
  avgPrice: number;        // notional-weighted avg, 0 if no orders
  orderCount: number;
  completed: boolean;      // maxTotal reached
  orders: DcaOrder[];      // per-market recent orders (bounded)
};

export type DcaPortfolioState = {
  config: DcaPortfolioConfig;
  status: "running" | "completed" | "idle";
  progress: MarketProgress[];
  globalOrderSeq: number;
  completedMarkets: number;
  startedAt: number;
};

const MAX_ORDERS_PER_MARKET = 20;

export function initState(config: DcaPortfolioConfig, now: number): DcaPortfolioState {
  const progress: MarketProgress[] = config.markets.map((market, i) => ({
    market,
    currentPrice: config.startingPrices[i] ?? config.startingPrices[0] ?? 0.15,
    lastOrderAt: null,
    totalQty: 0,
    totalNotional: 0,
    avgPrice: 0,
    orderCount: 0,
    completed: false,
    orders: [],
  }));
  return {
    config,
    status: "running",
    progress,
    globalOrderSeq: 0,
    completedMarkets: 0,
    startedAt: now,
  };
}

/**
 * Advance one tick. The external `price` argument is intentionally
 * ignored — every market runs its own internal GBM step in the store
 * layer, and the resulting mid gets fed back in via `progress[i].currentPrice`
 * before this function is called. Placement is driven purely by
 * `intervalSecs` per market.
 */
export function step(
  state: DcaPortfolioState,
  _priceIgnored: number,
  now: number,
): { state: DcaPortfolioState; events: string[] } {
  if (state.status !== "running") return { state, events: [] };
  const events: string[] = [];
  const { config } = state;
  const intervalMs = config.intervalSecs * 1000;

  for (const mp of state.progress) {
    if (mp.completed) continue;

    const due =
      mp.lastOrderAt === null
        ? now - state.startedAt >= intervalMs
        : now - mp.lastOrderAt >= intervalMs;
    if (!due) continue;

    const mid = mp.currentPrice;
    if (!(mid > 0)) continue;

    const priceMult = 1 + config.priceOffsetPct / 100;
    const orderPrice = round8(mid * priceMult);

    // Cap this order's qty by maxTotal, if configured
    let qty = config.amountPerOrder;
    if (config.maxTotal !== null) {
      const remaining = config.maxTotal - mp.totalQty;
      if (remaining <= 0) {
        mp.completed = true;
        state.completedMarkets += 1;
        events.push(`DONE [${mp.market}]: reached max total ${config.maxTotal}`);
        continue;
      }
      if (remaining < qty) qty = remaining;
    }
    if (qty <= 0) continue;

    const notional = round8(orderPrice * qty);
    state.globalOrderSeq += 1;
    const order: DcaOrder = {
      seq: state.globalOrderSeq,
      t: now,
      market: mp.market,
      side: config.side,
      type: config.side === "buy" ? "BID" : "OFFER",
      price: orderPrice,
      qty,
      notional,
      mid,
    };
    mp.orders.push(order);
    if (mp.orders.length > MAX_ORDERS_PER_MARKET) mp.orders.shift();
    mp.orderCount += 1;
    mp.totalQty = round8(mp.totalQty + qty);
    mp.totalNotional = round8(mp.totalNotional + notional);
    mp.avgPrice = mp.totalQty > 0 ? round8(mp.totalNotional / mp.totalQty) : 0;
    mp.lastOrderAt = now;

    events.push(
      `DCA #${order.seq} [${mp.market}]: ${order.type} ${fmt(qty)} @ ${fmt(orderPrice)} (mid=${fmt(mid)}, offset=${sgn(config.priceOffsetPct)}${config.priceOffsetPct.toFixed(2)}%)`,
    );

    if (config.maxTotal !== null && mp.totalQty >= config.maxTotal - 1e-12) {
      mp.completed = true;
      state.completedMarkets += 1;
      events.push(`DONE [${mp.market}]: reached max total ${config.maxTotal}`);
    }
  }

  if (state.completedMarkets >= state.config.markets.length && state.config.maxTotal !== null) {
    state.status = "completed";
    events.push(`DCA PORTFOLIO COMPLETE: all ${state.config.markets.length} markets reached max total`);
  }

  return { state, events };
}

function round8(n: number): number {
  return Math.round(n * 1e8) / 1e8;
}

function fmt(n: number): string {
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? 4 : 6;
  return n.toFixed(digits);
}

function sgn(n: number): string {
  return n >= 0 ? "+" : "";
}
