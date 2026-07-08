// Port of agent-market-abuse-spoof-layer detection logic to TypeScript. Mirrors
// crates/agent-market-abuse-spoof-layer/src/main.rs (handle_order + check_layering).
//
// Two rules:
//   Spoofing — burst of fast cancels: >= spoofBurst cancels within
//     spoofBurstWindowSecs, where each cancel landed <= spoofWindowSecs after
//     its create.
//   Layering — >= layerMinOrders open orders on the same market+side,
//     clustered within layerPriceBandPct of one another.
//
// Self-audit: order flow is simulated (order arrival + cancel rate),
// optionally augmented with a periodic spoof scenario.

export type Side = "BID" | "OFFER";

export type Order = {
  seq: number;
  t: number;              // epoch ms
  market: string;
  side: Side;
  price: number;
  qty: number;
  createdAt: number;      // == t
  cancelledAt?: number;
  status: "active" | "cancelled";
};

export type Alert = Readonly<{
  t: number;
  kind: "spoof" | "layer";
  details: string;
  ordersInvolved: number[];
}>;

export type MarketAbuseConfig = Readonly<{
  market: string;
  spoofBurst: number;               // default 10
  spoofBurstWindowSecs: number;     // default 10
  spoofWindowSecs: number;          // default 1
  layerMinOrders: number;           // default 5
  layerPriceBandPct: number;        // default 1.0
  orderArrivalPerTick: number;      // default 0.4 (Poisson mean)
  cancelRatePerTick: number;        // default 0.1 (fraction of active orders)
  spoofScenarioEnabled: boolean;    // default true
  startingPrice: number;            // seed for the price walk
}>;

export type MarketAbuseState = {
  config: MarketAbuseConfig;
  status: "monitoring" | "idle";
  currentPrice: number;
  activeOrders: Order[];             // still on book
  allOrders: Order[];                // bounded ~100
  alerts: Alert[];                   // bounded ~30
  spoofsDetected: number;
  layersDetected: number;
  ticksElapsed: number;
  ordersCreated: number;
  ordersCancelled: number;
  lastAlertAt?: number;
  // internal: scenario scheduler + pending cancels
  nextSpoofAt: number;
  scheduledCancels: Array<{ orderSeq: number; at: number }>;
};

const MAX_ALL_ORDERS = 100;
const MAX_ALERTS = 30;
const SPOOF_SCENARIO_EVERY_SECS = 30;

export function initState(config: MarketAbuseConfig, startPrice: number): MarketAbuseState {
  return {
    config,
    status: "monitoring",
    currentPrice: startPrice,
    activeOrders: [],
    allOrders: [],
    alerts: [],
    spoofsDetected: 0,
    layersDetected: 0,
    ticksElapsed: 0,
    ordersCreated: 0,
    ordersCancelled: 0,
    nextSpoofAt: Date.now() + SPOOF_SCENARIO_EVERY_SECS * 1000,
    scheduledCancels: [],
  };
}

// Poisson(lambda) sampler (Knuth). Good enough for small lambda in a demo.
function samplePoisson(lambda: number): number {
  if (lambda <= 0) return 0;
  const L = Math.exp(-lambda);
  let k = 0;
  let p = 1;
  while (true) {
    k += 1;
    p *= Math.random();
    if (p <= L) return k - 1;
  }
}

export function step(state: MarketAbuseState, price: number, now: number): { state: MarketAbuseState; events: string[] } {
  if (state.status !== "monitoring" || price <= 0) return { state, events: [] };
  const events: string[] = [];
  state.currentPrice = price;
  state.ticksElapsed += 1;
  const cfg = state.config;

  // 1) Fire scheduled spoof-scenario cancels (they hit before random cancels).
  if (state.scheduledCancels.length > 0) {
    const remaining: Array<{ orderSeq: number; at: number }> = [];
    for (const sc of state.scheduledCancels) {
      if (sc.at <= now) {
        cancelOrderBySeq(state, sc.orderSeq, now);
      } else {
        remaining.push(sc);
      }
    }
    state.scheduledCancels = remaining;
  }

  // 2) Order arrival (Poisson).
  const arrivals = samplePoisson(cfg.orderArrivalPerTick);
  for (let i = 0; i < arrivals; i++) {
    createOrder(state, now, randomSide(), randomPriceAround(price));
  }

  // 3) Random cancel rate.
  const activeCopy = [...state.activeOrders];
  for (const o of activeCopy) {
    if (Math.random() < cfg.cancelRatePerTick) {
      cancelOrder(state, o, now);
    }
  }

  // 4) Optional spoof-scenario: enqueue a synthetic burst that will trigger
  // detection. Creates spoofBurst orders now, schedules cancels 500-1000 ms
  // ahead so `cancelledAt - createdAt <= spoofWindowSecs`.
  if (cfg.spoofScenarioEnabled && now >= state.nextSpoofAt) {
    const side = Math.random() < 0.5 ? "BID" : "OFFER";
    for (let i = 0; i < cfg.spoofBurst; i++) {
      const o = createOrder(state, now, side, randomPriceAround(price));
      const delay = 400 + Math.floor(Math.random() * 500); // 400..900ms
      state.scheduledCancels.push({ orderSeq: o.seq, at: now + delay });
    }
    events.push(`SCENARIO: injected spoof burst of ${cfg.spoofBurst} ${side} orders (will cancel in <1s)`);
    state.nextSpoofAt = now + SPOOF_SCENARIO_EVERY_SECS * 1000;
  }

  // 5) Detection: spoofing. Count cancels whose (cancelledAt - createdAt) <= spoofWindowSecs
  // and cancelledAt within the last spoofBurstWindowSecs. If >= spoofBurst -> alert.
  const cutoff = now - cfg.spoofBurstWindowSecs * 1000;
  const fastCancels = state.allOrders.filter((o) =>
    o.status === "cancelled" &&
    o.cancelledAt !== undefined &&
    o.cancelledAt >= cutoff &&
    (o.cancelledAt - o.createdAt) <= cfg.spoofWindowSecs * 1000,
  );
  if (fastCancels.length >= cfg.spoofBurst) {
    // Avoid alert spam: only if the most recent alert isn't a fresh spoof.
    const last = state.alerts[state.alerts.length - 1];
    const cooldownOk = !last || last.kind !== "spoof" || (now - last.t) >= cfg.spoofBurstWindowSecs * 1000;
    if (cooldownOk) {
      const involved = fastCancels.map((o) => o.seq);
      const alert: Alert = {
        t: now,
        kind: "spoof",
        details: `${fastCancels.length} fast cancels in last ${cfg.spoofBurstWindowSecs}s (threshold ${cfg.spoofBurst})`,
        ordersInvolved: involved,
      };
      pushAlert(state, alert);
      state.spoofsDetected += 1;
      state.lastAlertAt = now;
      events.push(`SPOOF ALERT: ${alert.details}`);
    }
  }

  // 6) Detection: layering. Group active orders by (market, side), then look
  // for a cluster of >= layerMinOrders whose price spread is <= layerPriceBandPct.
  const groups = new Map<string, Order[]>();
  for (const o of state.activeOrders) {
    const k = `${o.market}|${o.side}`;
    const arr = groups.get(k) ?? [];
    arr.push(o);
    groups.set(k, arr);
  }
  for (const [key, arr] of groups) {
    if (arr.length < cfg.layerMinOrders) continue;
    const prices = arr.map((o) => o.price).sort((a, b) => a - b);
    const lo = prices[0];
    const hi = prices[prices.length - 1];
    if (lo <= 0) continue;
    const spreadPct = ((hi - lo) / lo) * 100;
    if (spreadPct <= cfg.layerPriceBandPct) {
      const last = state.alerts[state.alerts.length - 1];
      const cooldownOk = !last || last.kind !== "layer" || (now - last.t) >= 5000;
      if (cooldownOk) {
        const involved = arr.map((o) => o.seq);
        const alert: Alert = {
          t: now,
          kind: "layer",
          details: `${arr.length} ${key.split("|")[1]} orders on ${key.split("|")[0]} within ${spreadPct.toFixed(3)}% band`,
          ordersInvolved: involved,
        };
        pushAlert(state, alert);
        state.layersDetected += 1;
        state.lastAlertAt = now;
        events.push(`LAYER ALERT: ${alert.details}`);
      }
    }
  }

  return { state, events };
}

function createOrder(state: MarketAbuseState, now: number, side: Side, price: number): Order {
  state.ordersCreated += 1;
  const seq = state.ordersCreated;
  const o: Order = {
    seq,
    t: now,
    market: state.config.market,
    side,
    price: round8(price),
    qty: 1,
    createdAt: now,
    status: "active",
  };
  state.activeOrders.push(o);
  state.allOrders.push(o);
  if (state.allOrders.length > MAX_ALL_ORDERS) state.allOrders.shift();
  return o;
}

function cancelOrder(state: MarketAbuseState, o: Order, now: number): void {
  if (o.status !== "active") return;
  o.status = "cancelled";
  o.cancelledAt = now;
  state.activeOrders = state.activeOrders.filter((x) => x.seq !== o.seq);
  state.ordersCancelled += 1;
}

function cancelOrderBySeq(state: MarketAbuseState, seq: number, now: number): void {
  const o = state.activeOrders.find((x) => x.seq === seq);
  if (o) cancelOrder(state, o, now);
}

function pushAlert(state: MarketAbuseState, a: Alert): void {
  state.alerts.push(a);
  if (state.alerts.length > MAX_ALERTS) state.alerts.shift();
}

function randomSide(): Side {
  return Math.random() < 0.5 ? "BID" : "OFFER";
}

function randomPriceAround(mid: number): number {
  // Uniform ±2% around mid.
  const jitter = (Math.random() - 0.5) * 0.04;
  return mid * (1 + jitter);
}

function round8(n: number): number {
  return Math.round(n * 1e8) / 1e8;
}
