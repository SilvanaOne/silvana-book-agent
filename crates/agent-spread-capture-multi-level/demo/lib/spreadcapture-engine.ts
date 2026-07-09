// Port of agent-spread-capture-multi-level placement logic to TypeScript. Mirrors
// crates/agent-spread-capture-multi-level/src/main.rs (fn sc_loop).
//
// Rule (per refresh cycle):
//   1. Cancel any live BID / OFFER quotes from previous cycle.
//   2. Build a ladder of `levels` per side around the mid.
//        offset_i = inner_bps / 20000 + step_bps / 10000 × (i − 1)
//        bid_i    = mid × (1 − offset_i)
//        offer_i  = mid × (1 + offset_i)
//   3. Inventory clamp:
//        net > +maxInventory → skip the whole BID side
//        net < −maxInventory → skip the whole OFFER side
//   4. Submit surviving levels at `quantityPerLevel`.
//
// Fill detection between cycles (simulated tape): a BID is filled when the mid
// dips at or below its price; an OFFER when the mid rises at or above its
// price. Filling a BID increases net inventory by qty; an OFFER decreases it.
// Realized PnL is booked on OFFER fills against a rolling weighted-average
// bid-cost basis (buy-low / sell-high spread capture).

export type OrderType = "BID" | "OFFER";

export type SpreadCaptureConfig = Readonly<{
  market: string;
  levels: number;           // number of levels per side, >= 1
  innerSpreadBps: number;   // level-1 half-spread in bps per side
  stepBps: number;          // extra bps added per additional level
  quantityPerLevel: number; // qty per quote
  maxInventory: number;     // inventory clamp (absolute value)
  refreshSecs: number;      // refresh interval in seconds
  startingPrice: number;    // seed for the price simulator
}>;

export type SpreadCaptureOrder = {
  seq: number;
  level: number;
  t: number;                // epoch ms of placement
  side: OrderType;
  price: number;
  qty: number;
  status: "open" | "filled" | "cancelled";
  filledAt?: number;
  filledMid?: number;
};

export type ActiveQuote = Readonly<{ seq: number; level: number; side: OrderType; price: number; qty: number; t: number }>;

export type SpreadCaptureState = {
  config: SpreadCaptureConfig;
  status: "monitoring" | "idle";
  currentPrice: number;
  netInventory: number;
  lastRefreshAt: number;
  cyclesRefreshed: number;
  ordersPlaced: number;
  ordersFilled: number;
  bidFills: number;
  offerFills: number;
  realizedPnl: number;
  spreadCapturedCum: number;
  avgSpreadBps: number;
  spreadSamples: number;
  bidLadder: ActiveQuote[];   // active bids sorted by level asc (closest to mid first)
  offerLadder: ActiveQuote[]; // active offers sorted by level asc
  bidCostBasis: number;
  bidCostQty: number;
  orders: SpreadCaptureOrder[];
};

const MAX_ORDERS = 200;

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
    bidFills: 0,
    offerFills: 0,
    realizedPnl: 0,
    spreadCapturedCum: 0,
    avgSpreadBps: 0,
    spreadSamples: 0,
    bidLadder: [],
    offerLadder: [],
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

  // --- Fill detection over the active ladder. Bids fill when mid dips at/below,
  //     offers fill when mid rises at/above. We consume levels closest-to-mid
  //     first so the on-book footprint shrinks in a realistic order.
  {
    const remainingBids: ActiveQuote[] = [];
    // Sort by descending price for bids (highest bid closest to mid).
    const sortedBids = [...state.bidLadder].sort((a, b) => b.price - a.price);
    for (const q of sortedBids) {
      if (price <= q.price) {
        markFilled(state, q.seq, now, price);
        const newHeld = state.bidCostQty + q.qty;
        state.bidCostBasis = newHeld > 0
          ? (state.bidCostBasis * state.bidCostQty + q.price * q.qty) / newHeld
          : 0;
        state.bidCostQty = newHeld;
        state.netInventory += q.qty;
        state.ordersFilled += 1;
        state.bidFills += 1;
        events.push(`FILL #${q.seq} BID L${q.level} ${q.qty} @ ${fmt(q.price)} (mid=${fmt(price)}, inv=${fmt(state.netInventory)})`);
      } else {
        remainingBids.push(q);
      }
    }
    state.bidLadder = remainingBids.sort((a, b) => a.level - b.level);
  }
  {
    const remainingOffers: ActiveQuote[] = [];
    // Sort by ascending price for offers (lowest offer closest to mid).
    const sortedOffers = [...state.offerLadder].sort((a, b) => a.price - b.price);
    for (const q of sortedOffers) {
      if (price >= q.price) {
        markFilled(state, q.seq, now, price);
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
        state.offerFills += 1;
        events.push(`FILL #${q.seq} OFFER L${q.level} ${q.qty} @ ${fmt(q.price)} (mid=${fmt(price)}, captured=${sgn(captured)}${fmt(Math.abs(captured))})`);
      } else {
        remainingOffers.push(q);
      }
    }
    state.offerLadder = remainingOffers.sort((a, b) => a.level - b.level);
  }

  // --- Refresh cycle?
  const dueAt = state.lastRefreshAt + state.config.refreshSecs * 1000;
  if (state.lastRefreshAt === 0 || now >= dueAt) {
    // Cancel surviving levels.
    for (const q of state.bidLadder) {
      cancelActive(state, q.seq);
      events.push(`CANCEL #${q.seq} BID L${q.level} @ ${fmt(q.price)}`);
    }
    for (const q of state.offerLadder) {
      cancelActive(state, q.seq);
      events.push(`CANCEL #${q.seq} OFFER L${q.level} @ ${fmt(q.price)}`);
    }
    state.bidLadder = [];
    state.offerLadder = [];

    const placeBidSide = state.netInventory < state.config.maxInventory;
    const placeOfferSide = state.netInventory > -state.config.maxInventory;

    if (!placeBidSide) events.push(`SKIP BID side (inv=${fmt(state.netInventory)} ≥ +${state.config.maxInventory})`);
    if (!placeOfferSide) events.push(`SKIP OFFER side (inv=${fmt(state.netInventory)} ≤ −${state.config.maxInventory})`);

    for (let i = 1; i <= state.config.levels; i++) {
      const offset = state.config.innerSpreadBps / 20000 + (state.config.stepBps / 10000) * (i - 1);
      const bidPx = round8(price * (1 - offset));
      const offerPx = round8(price * (1 + offset));

      if (placeBidSide && bidPx > 0) {
        const seq = ++state.ordersPlaced;
        const o: SpreadCaptureOrder = { seq, level: i, t: now, side: "BID", price: bidPx, qty: state.config.quantityPerLevel, status: "open" };
        state.orders.push(o);
        state.bidLadder.push({ seq, level: i, side: "BID", price: bidPx, qty: state.config.quantityPerLevel, t: now });
        events.push(`QUOTE BID L${i} #${seq} ${o.qty} @ ${fmt(bidPx)} (offset=${(offset * 10000).toFixed(1)} bps)`);
      }
      if (placeOfferSide) {
        const seq = ++state.ordersPlaced;
        const o: SpreadCaptureOrder = { seq, level: i, t: now, side: "OFFER", price: offerPx, qty: state.config.quantityPerLevel, status: "open" };
        state.orders.push(o);
        state.offerLadder.push({ seq, level: i, side: "OFFER", price: offerPx, qty: state.config.quantityPerLevel, t: now });
        events.push(`QUOTE OFFER L${i} #${seq} ${o.qty} @ ${fmt(offerPx)} (offset=${(offset * 10000).toFixed(1)} bps)`);
      }
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
