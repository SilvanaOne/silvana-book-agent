// Port of agent-order-expiry-price-drift logic to TypeScript. Mirrors
// crates/agent-order-expiry-price-drift/src/main.rs (fn run_loop / fn sweep).
//
// Rule:
//   Every check-interval seconds, scan this party's active orders.
//   For each order compute drift_pct = |order.price - mid| / mid * 100.
//   If drift_pct > max_drift_pct → cancel it.
//   With --dry-run, log what would be cancelled without doing it.
//
// For the demo we synthesize a mock stream of own orders: on every tick
// each unit-of-time draws a Poisson-like arrival with rate `orderArrivalPerTick`,
// picks a random side, and posts near mid (BID slightly below, OFFER slightly
// above). The synthetic mid then walks over time (see price-simulator), so
// orders placed earlier can end up far from the current mid and become
// candidates for cancellation.

export type OrderType = "BID" | "OFFER";

export type OrderExpiryConfig = Readonly<{
  market: string;
  maxDriftPct: number;         // cancel orders whose |price-mid|/mid*100 exceeds this
  checkIntervalSecs: number;   // how often the sweep runs
  orderArrivalPerTick: number; // mock: average number of new own orders per tick
  dryRun: boolean;             // log-only mode
  startingPrice: number;
}>;

export type OrderExpiryOrder = {
  seq: number;
  createdAt: number;           // epoch ms
  side: OrderType;
  price: number;
  qty: number;
  market: string;
  status: "active" | "cancelled";
  cancelledAt?: number;
  midAtCancel?: number;
  driftAtCancelPct?: number;
};

export type OrderExpiryState = {
  config: OrderExpiryConfig;
  status: "monitoring" | "idle";
  currentPrice: number;
  orders: OrderExpiryOrder[];
  nextSeq: number;
  checksCount: number;
  cancelledCount: number;
  arrivedCount: number;
  lastCheckAt?: number;
  startedAt: number;
  maxDriftEverPct: number;     // largest drift we ever cancelled at
  sumDriftAtCancel: number;    // for computing average
  lastCancelReason?: string;
};

const MAX_ORDERS = 200;

export function initState(config: OrderExpiryConfig, startPrice: number, now: number): OrderExpiryState {
  return {
    config,
    status: "monitoring",
    currentPrice: startPrice,
    orders: [],
    nextSeq: 1,
    checksCount: 0,
    cancelledCount: 0,
    arrivedCount: 0,
    lastCheckAt: undefined,
    startedAt: now,
    maxDriftEverPct: 0,
    sumDriftAtCancel: 0,
    lastCancelReason: undefined,
  };
}

/** Applies a tick. Returns state + emitted events. */
export function step(state: OrderExpiryState, price: number, now: number): { state: OrderExpiryState; events: string[] } {
  if (state.status !== "monitoring" || price <= 0) return { state, events: [] };
  const events: string[] = [];
  state.currentPrice = price;

  // ── Simulate mock arrivals of the agent's own orders ──────────────────────
  // Draw a Poisson-like count using orderArrivalPerTick as the mean. New orders
  // are placed close to the CURRENT mid; drift accumulates only after the mid
  // walks away.
  const arrivals = poisson(state.config.orderArrivalPerTick);
  for (let i = 0; i < arrivals; i++) {
    const side: OrderType = Math.random() < 0.5 ? "BID" : "OFFER";
    // Post a bit inside the spread: BID slightly below mid, OFFER slightly above.
    const jitter = (Math.random() * 0.006) + 0.001; // 0.1% .. 0.7%
    const orderPrice = round8(price * (side === "BID" ? 1 - jitter : 1 + jitter));
    const order: OrderExpiryOrder = {
      seq: state.nextSeq++,
      createdAt: now,
      side,
      price: orderPrice > 0 ? orderPrice : price,
      qty: 1,
      market: state.config.market,
      status: "active",
    };
    state.orders.push(order);
    state.arrivedCount++;
    events.push(`NEW ORDER #${order.seq} ${side} 1 @ ${fmt(order.price)} (mid ${fmt(price)})`);
  }

  // Bound memory: keep active + latest 40 cancelled.
  if (state.orders.length > MAX_ORDERS) {
    state.orders = state.orders.filter((o) => o.status === "active").concat(
      state.orders.filter((o) => o.status === "cancelled").slice(-40),
    );
  }

  // ── Sweep: every checkIntervalSecs ────────────────────────────────────────
  const shouldSweep = state.lastCheckAt === undefined
    ? (now - state.startedAt) >= state.config.checkIntervalSecs * 1000
    : (now - state.lastCheckAt) >= state.config.checkIntervalSecs * 1000;
  if (shouldSweep) {
    state.lastCheckAt = now;
    state.checksCount++;
    let scanned = 0;
    let drifted = 0;
    for (const o of state.orders) {
      if (o.status !== "active") continue;
      scanned++;
      if (price <= 0) continue;
      const driftPct = Math.abs(o.price - price) / price * 100;
      if (driftPct <= state.config.maxDriftPct) continue;
      drifted++;
      o.status = "cancelled";
      o.cancelledAt = now;
      o.midAtCancel = price;
      o.driftAtCancelPct = driftPct;
      state.cancelledCount++;
      state.sumDriftAtCancel += driftPct;
      if (driftPct > state.maxDriftEverPct) state.maxDriftEverPct = driftPct;
      const prefix = state.config.dryRun ? "[DRY-RUN] would cancel" : "CANCELLED";
      const reason = `${o.side} @ ${fmt(o.price)} vs mid ${fmt(price)} → drift ${driftPct.toFixed(2)}% > ${state.config.maxDriftPct}%`;
      state.lastCancelReason = reason;
      events.push(`${prefix} #${o.seq} ${reason}`);
    }
    events.push(`sweep #${state.checksCount}: scanned=${scanned} drifted=${drifted} mid=${fmt(price)}`);
  }

  return { state, events };
}

// Knuth Poisson sampler.
function poisson(lambda: number): number {
  if (lambda <= 0) return 0;
  if (lambda > 30) {
    // Fallback: clamp to avoid runaway.
    return Math.round(lambda);
  }
  const L = Math.exp(-lambda);
  let k = 0;
  let p = 1;
  do {
    k++;
    p *= Math.random();
  } while (p > L);
  return k - 1;
}

function round8(n: number): number {
  return Math.round(n * 1e8) / 1e8;
}

function fmt(n: number): string {
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? 4 : 6;
  return n.toFixed(digits);
}
