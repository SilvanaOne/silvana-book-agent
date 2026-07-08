// Port of agent-order-matching-trigger placement logic to TypeScript. Mirrors
// crates/agent-order-matching-trigger/src/main.rs (fn match_loop).
//
// Rules (from the Rust agent):
//   * synthesize a top-of-book from the current mid using bookSpreadBps:
//       best_bid   = mid * (1 - bookSpreadBps / 20000)
//       best_offer = mid * (1 + bookSpreadBps / 20000)
//   * if buyTrigger  != null AND best_offer <= buyTrigger
//         AND no open BID snipe → place BID @ best_offer, qty = config.quantity
//   * if sellTrigger != null AND best_bid  >= sellTrigger
//         AND no open OFFER snipe → place OFFER @ best_bid, qty = config.quantity
//   * At most one open snipe per side at a time.
//
// Fill sim: a BID at price p fills when the mid drifts <= p; an OFFER at
// price p fills when the mid drifts >= p. Fill/cancel unlocks that side.

export type OrderType = "BID" | "OFFER";

export type OrderMatchingConfig = Readonly<{
  market: string;
  buyTrigger: number | null;
  sellTrigger: number | null;
  quantity: number;
  bookSpreadBps: number;   // half-spread applied both sides, in bps of mid
  startingPrice: number;
}>;

export type SnipeOrder = Readonly<{
  seq: number;
  t: number;                // epoch ms when placed
  side: OrderType;
  price: number;            // snipe execution price = best_offer (BID) / best_bid (OFFER)
  qty: number;
  trigger: number;          // trigger threshold at signal time
  status: "open" | "filled" | "cancelled";
  filledAt?: number;
  filledMid?: number;
}>;

export type OrderMatchingState = {
  config: OrderMatchingConfig;
  status: "monitoring" | "idle";
  currentPrice: number;
  bestBid: number | null;
  bestOffer: number | null;
  buySnipeOpen: SnipeOrder | null;
  sellSnipeOpen: SnipeOrder | null;
  snipesPlaced: number;
  snipesFilled: number;
  realizedPnl: number;
  orders: SnipeOrder[];    // bounded (~40)
};

const MAX_ORDERS = 40;

export function initState(config: OrderMatchingConfig, startPrice: number): OrderMatchingState {
  const halfBps = config.bookSpreadBps / 20000;
  return {
    config,
    status: "monitoring",
    currentPrice: startPrice,
    bestBid: startPrice * (1 - halfBps),
    bestOffer: startPrice * (1 + halfBps),
    buySnipeOpen: null,
    sellSnipeOpen: null,
    snipesPlaced: 0,
    snipesFilled: 0,
    realizedPnl: 0,
    orders: [],
  };
}

/** Applies a tick. Returns state + emitted events. */
export function step(
  state: OrderMatchingState,
  price: number,
  now: number,
): { state: OrderMatchingState; events: string[] } {
  if (state.status !== "monitoring" || price <= 0) return { state, events: [] };
  const events: string[] = [];
  state.currentPrice = price;

  const halfBps = state.config.bookSpreadBps / 20000;
  const bestBid = price * (1 - halfBps);
  const bestOffer = price * (1 + halfBps);
  state.bestBid = bestBid;
  state.bestOffer = bestOffer;

  // Fill detection on open snipes.
  //   BID  → fills when mid <= bid.price  (offer was below trigger, seller hits)
  //   OFFER→ fills when mid >= offer.price
  if (state.buySnipeOpen && state.buySnipeOpen.status === "open") {
    const b = state.buySnipeOpen;
    if (price <= b.price) {
      const filled: SnipeOrder = { ...b, status: "filled", filledAt: now, filledMid: price };
      state.buySnipeOpen = null;
      state.snipesFilled += 1;
      // BID snipe = we bought at b.price, marked to current mid.
      const pnl = (price - b.price) * b.qty;
      state.realizedPnl += pnl;
      // Update the entry in the bounded log too.
      const idx = state.orders.findIndex((o) => o.seq === b.seq);
      if (idx >= 0) state.orders[idx] = filled;
      events.push(
        `FILL BID #${b.seq}: ${b.qty} @ ${fmt(b.price)} (mid=${fmt(price)}, pnl=${sgn(pnl)}${fmt(Math.abs(pnl))})`,
      );
    }
  }
  if (state.sellSnipeOpen && state.sellSnipeOpen.status === "open") {
    const s = state.sellSnipeOpen;
    if (price >= s.price) {
      const filled: SnipeOrder = { ...s, status: "filled", filledAt: now, filledMid: price };
      state.sellSnipeOpen = null;
      state.snipesFilled += 1;
      // OFFER snipe = we sold at s.price, marked to current mid.
      const pnl = (s.price - price) * s.qty;
      state.realizedPnl += pnl;
      const idx = state.orders.findIndex((o) => o.seq === s.seq);
      if (idx >= 0) state.orders[idx] = filled;
      events.push(
        `FILL OFFER #${s.seq}: ${s.qty} @ ${fmt(s.price)} (mid=${fmt(price)}, pnl=${sgn(pnl)}${fmt(Math.abs(pnl))})`,
      );
    }
  }

  // Trigger checks.
  const buyTrig = state.config.buyTrigger;
  if (buyTrig !== null && state.buySnipeOpen === null && bestOffer <= buyTrig) {
    const seq = state.snipesPlaced + 1;
    const order: SnipeOrder = {
      seq,
      t: now,
      side: "BID",
      price: round8(bestOffer),
      qty: state.config.quantity,
      trigger: buyTrig,
      status: "open",
    };
    state.buySnipeOpen = order;
    state.orders.push(order);
    state.snipesPlaced = seq;
    if (state.orders.length > MAX_ORDERS) state.orders.shift();
    events.push(
      `SNIPE BID #${seq}: bought at ${fmt(order.price)} (trigger ${fmt(buyTrig)}, best_offer=${fmt(bestOffer)}, qty ${order.qty})`,
    );
  }

  const sellTrig = state.config.sellTrigger;
  if (sellTrig !== null && state.sellSnipeOpen === null && bestBid >= sellTrig) {
    const seq = state.snipesPlaced + 1;
    const order: SnipeOrder = {
      seq,
      t: now,
      side: "OFFER",
      price: round8(bestBid),
      qty: state.config.quantity,
      trigger: sellTrig,
      status: "open",
    };
    state.sellSnipeOpen = order;
    state.orders.push(order);
    state.snipesPlaced = seq;
    if (state.orders.length > MAX_ORDERS) state.orders.shift();
    events.push(
      `SNIPE OFFER #${seq}: sold at ${fmt(order.price)} (trigger ${fmt(sellTrig)}, best_bid=${fmt(bestBid)}, qty ${order.qty})`,
    );
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
