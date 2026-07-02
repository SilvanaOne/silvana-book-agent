// Port of agent-order-expiry logic to TypeScript. Mirrors
// crates/agent-order-expiry/src/main.rs (fn run_loop / fn sweep).
//
// Rule:
//   Every check-interval seconds, scan this party's active orders.
//   For each order where (now - createdAt) > max_age_secs → cancel it.
//   With --dry-run, log what would be cancelled without doing it.
//
// For the demo we synthesize a mock stream of own orders: on every tick
// each unit-of-time draws a Poisson-like arrival with rate `orderArrivalPerTick`,
// picks a random side, and posts at mid ± ~2%.

export type OrderType = "BID" | "OFFER";

export type OrderExpiryConfig = Readonly<{
  market: string;
  maxAgeSecs: number;         // TTL — cancel orders older than this
  checkIntervalSecs: number;  // how often the sweep runs
  orderArrivalPerTick: number; // mock: average number of new own orders per tick
  dryRun: boolean;            // log-only mode
  startingPrice: number;
}>;

export type OrderExpiryOrder = {
  seq: number;
  createdAt: number;          // epoch ms
  side: OrderType;
  price: number;
  qty: number;
  market: string;
  status: "active" | "cancelled";
  cancelledAt?: number;
  ageAtCancelSecs?: number;
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
  oldestEverCancelledSecs: number;
  sumAgeAtCancel: number;      // for computing average
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
    oldestEverCancelledSecs: 0,
    sumAgeAtCancel: 0,
  };
}

/** Applies a tick. Returns state + emitted events. */
export function step(state: OrderExpiryState, price: number, now: number): { state: OrderExpiryState; events: string[] } {
  if (state.status !== "monitoring" || price <= 0) return { state, events: [] };
  const events: string[] = [];
  state.currentPrice = price;

  // ── Simulate mock arrivals of the agent's own orders ──────────────────────
  // Draw a Poisson-like count using orderArrivalPerTick as the mean.
  const arrivals = poisson(state.config.orderArrivalPerTick);
  for (let i = 0; i < arrivals; i++) {
    const side: OrderType = Math.random() < 0.5 ? "BID" : "OFFER";
    const offset = 1 + (Math.random() * 0.04 - 0.02);         // ±2%
    const orderPrice = round8(price * (side === "BID" ? offset - 0.02 : offset + 0.02));
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
    events.push(`NEW ORDER #${order.seq} ${side} 1 @ ${fmt(order.price)}`);
  }

  // Bound memory: drop oldest cancelled ones if we exceed the cap.
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
    let expired = 0;
    for (const o of state.orders) {
      if (o.status !== "active") continue;
      scanned++;
      const ageSecs = Math.floor((now - o.createdAt) / 1000);
      if (ageSecs <= state.config.maxAgeSecs) continue;
      expired++;
      o.status = "cancelled";
      o.cancelledAt = now;
      o.ageAtCancelSecs = ageSecs;
      state.cancelledCount++;
      state.sumAgeAtCancel += ageSecs;
      if (ageSecs > state.oldestEverCancelledSecs) state.oldestEverCancelledSecs = ageSecs;
      const prefix = state.config.dryRun ? "[DRY-RUN] would cancel" : "CANCELLED";
      events.push(`${prefix} #${o.seq} ${o.side} @ ${fmt(o.price)} (age ${ageSecs}s > TTL ${state.config.maxAgeSecs}s)`);
    }
    events.push(`sweep #${state.checksCount}: scanned=${scanned} expired=${expired}`);
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
