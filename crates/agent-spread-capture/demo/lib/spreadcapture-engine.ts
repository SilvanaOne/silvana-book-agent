// Port of agent-spread-capture placement logic to TypeScript. Mirrors
// crates/agent-spread-capture/src/main.rs (fn sc_loop).
//
// Rule (per refresh cycle):
//   1. Cancel any live BID / OFFER from previous cycle.
//   2. bidPx   = mid × (1 − spread_bps / 20000)
//      offerPx = mid × (1 + spread_bps / 20000)
//   3. Inventory clamp:
//        net > +maxInventory → skip BID (already long)
//        net < −maxInventory → skip OFFER (already short)
//   4. Submit surviving side(s) at `quantity`.
//
// Fill detection between cycles (simulated tape): a BID is filled when the mid
// dips at or below its price; an OFFER when the mid rises at or above its
// price. Filling a BID increases net inventory by qty; an OFFER decreases it.
// Realized PnL is booked on OFFER fills against a rolling weighted-average
// bid-cost basis (buy-low / sell-high spread capture).

export type OrderType = "BID" | "OFFER";

export type SpreadCaptureConfig = Readonly<{
  market: string;
  spreadBps: number;      // half-spread in bps applied per side, e.g. 50 = 0.50% each side
  quantity: number;       // qty per order
  maxInventory: number;   // inventory clamp (absolute value)
  refreshSecs: number;    // refresh interval in seconds
  startingPrice: number;  // seed for the price simulator
}>;

export type SpreadCaptureOrder = Readonly<{
  seq: number;
  t: number;              // epoch ms of placement
  side: OrderType;
  price: number;
  qty: number;
  status: "open" | "filled" | "cancelled";
  filledAt?: number;
  filledMid?: number;
}>;

export type ActiveQuote = Readonly<{ price: number; qty: number; t: number; seq: number }>;

export type SpreadCaptureState = {
  config: SpreadCaptureConfig;
  status: "monitoring" | "idle";
  currentPrice: number;
  netInventory: number;    // filled_buy - filled_sell (base units)
  lastRefreshAt: number;   // epoch ms
  cyclesRefreshed: number;
  ordersPlaced: number;
  ordersFilled: number;
  realizedPnl: number;     // quote units, from paired bid→offer captures
  spreadCapturedCum: number; // cumulative sum of (offer_fill - bid_fill) × qty
  avgSpreadBps: number;    // running average captured spread in bps
  spreadSamples: number;
  bidActive: ActiveQuote | null;
  offerActive: ActiveQuote | null;
  bidCostBasis: number;    // weighted-average bid fill price (for pnl accounting)
  bidCostQty: number;      // accumulated base qty held from bid fills, unwound by offer fills
  orders: SpreadCaptureOrder[]; // bounded history
};

const MAX_ORDERS = 60;

export function initState(config: SpreadCaptureConfig, startPrice: number): SpreadCaptureState {
  return {
    config,
    status: "monitoring",
    currentPrice: startPrice,
    netInventory: 0,
    lastRefreshAt: 0,
    cyclesRefreshed: 0,
    ordersPlaced: 0,
    ordersFilled: 0,
    realizedPnl: 0,
    spreadCapturedCum: 0,
    avgSpreadBps: 0,
    spreadSamples: 0,
    bidActive: null,
    offerActive: null,
    bidCostBasis: 0,
    bidCostQty: 0,
    orders: [],
  };
}

/** Applies a tick. Returns state + emitted events. */
export function step(
  state: SpreadCaptureState,
  price: number,
  now: number,
): { state: SpreadCaptureState; events: string[] } {
  if (state.status !== "monitoring" || price <= 0) return { state, events: [] };
  const events: string[] = [];
  state.currentPrice = price;

  // Fill detection on live quotes against synthetic tape.
  if (state.bidActive && price <= state.bidActive.price) {
    const q = state.bidActive;
    markFilled(state, q.seq, now, price);
    // Update cost basis (VWAP over accumulated held qty).
    const newHeld = state.bidCostQty + q.qty;
    state.bidCostBasis = newHeld > 0
      ? (state.bidCostBasis * state.bidCostQty + q.price * q.qty) / newHeld
      : 0;
    state.bidCostQty = newHeld;
    state.netInventory += q.qty;
    state.ordersFilled += 1;
    events.push(`FILL #${q.seq} BID ${q.qty} @ ${fmt(q.price)} (mid=${fmt(price)}, inv=${fmt(state.netInventory)})`);
    state.bidActive = null;
  }
  if (state.offerActive && price >= state.offerActive.price) {
    const q = state.offerActive;
    markFilled(state, q.seq, now, price);
    // Pair against outstanding bid cost. If we have no bid-cost basis (short
    // sell), fall back to booking against price for a rough proxy.
    const pairQty = Math.min(state.bidCostQty, q.qty);
    let captured = 0;
    if (pairQty > 0) {
      captured = (q.price - state.bidCostBasis) * pairQty;
      state.bidCostQty -= pairQty;
      if (state.bidCostQty <= 1e-12) {
        state.bidCostQty = 0;
        state.bidCostBasis = 0;
      }
    }
    state.realizedPnl += captured;
    state.spreadCapturedCum += captured;
    if (pairQty > 0 && state.bidCostBasis > 0) {
      const bps = ((q.price - state.bidCostBasis) / state.bidCostBasis) * 10000;
      state.spreadSamples += 1;
      state.avgSpreadBps = state.avgSpreadBps + (bps - state.avgSpreadBps) / state.spreadSamples;
    }
    state.netInventory -= q.qty;
    state.ordersFilled += 1;
    events.push(`FILL #${q.seq} OFFER ${q.qty} @ ${fmt(q.price)} (mid=${fmt(price)}, captured=${sgn(captured)}${fmt(Math.abs(captured))})`);
    state.offerActive = null;
  }

  // Refresh cycle?
  const dueAt = state.lastRefreshAt + state.config.refreshSecs * 1000;
  if (state.lastRefreshAt === 0 || now >= dueAt) {
    // Cancel any surviving quotes (they were not filled during the cycle).
    if (state.bidActive) {
      cancelActive(state, state.bidActive.seq);
      events.push(`CANCEL #${state.bidActive.seq} BID @ ${fmt(state.bidActive.price)}`);
      state.bidActive = null;
    }
    if (state.offerActive) {
      cancelActive(state, state.offerActive.seq);
      events.push(`CANCEL #${state.offerActive.seq} OFFER @ ${fmt(state.offerActive.price)}`);
      state.offerActive = null;
    }

    const halfBps = state.config.spreadBps / 20000;
    const bidPx = round8(price * (1 - halfBps));
    const offerPx = round8(price * (1 + halfBps));
    const placeBid = state.netInventory < state.config.maxInventory;
    const placeOffer = state.netInventory > -state.config.maxInventory;

    if (placeBid) {
      const seq = ++state.ordersPlaced;
      const o: SpreadCaptureOrder = { seq, t: now, side: "BID", price: bidPx, qty: state.config.quantity, status: "open" };
      state.orders.push(o);
      state.bidActive = { seq, t: now, price: bidPx, qty: state.config.quantity };
      events.push(`QUOTE BID #${seq} ${o.qty} @ ${fmt(bidPx)} (mid=${fmt(price)}, spread=${state.config.spreadBps} bps)`);
    } else {
      events.push(`SKIP BID (inv=${fmt(state.netInventory)} ≥ +${state.config.maxInventory})`);
    }
    if (placeOffer) {
      const seq = ++state.ordersPlaced;
      const o: SpreadCaptureOrder = { seq, t: now, side: "OFFER", price: offerPx, qty: state.config.quantity, status: "open" };
      state.orders.push(o);
      state.offerActive = { seq, t: now, price: offerPx, qty: state.config.quantity };
      events.push(`QUOTE OFFER #${seq} ${o.qty} @ ${fmt(offerPx)} (mid=${fmt(price)}, spread=${state.config.spreadBps} bps)`);
    } else {
      events.push(`SKIP OFFER (inv=${fmt(state.netInventory)} ≤ −${state.config.maxInventory})`);
    }

    while (state.orders.length > MAX_ORDERS) state.orders.shift();
    state.lastRefreshAt = now;
    state.cyclesRefreshed += 1;
  }

  return { state, events };
}

function markFilled(state: SpreadCaptureState, seq: number, now: number, mid: number) {
  const idx = state.orders.findIndex((o) => o.seq === seq);
  if (idx >= 0) {
    const prev = state.orders[idx];
    state.orders[idx] = { ...prev, status: "filled", filledAt: now, filledMid: mid };
  }
}

function cancelActive(state: SpreadCaptureState, seq: number) {
  const idx = state.orders.findIndex((o) => o.seq === seq && o.status === "open");
  if (idx >= 0) {
    const prev = state.orders[idx];
    state.orders[idx] = { ...prev, status: "cancelled" };
  }
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
