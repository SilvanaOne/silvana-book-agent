// Port of agent-risk-management to TypeScript. Mirrors
// crates/agent-risk-management/src/main.rs (fn evaluate) — every cycle:
//  1. Query "own" orders + settlement proposals from a simulated book,
//  2. Compute open_notional and per-market breakdown,
//  3. Compare against the policy limits,
//  4. Emit a `risk.status` event with hits + (if enforce) cancel excess orders.
//
// The demo simulates a small trading book that drifts: each cycle a few
// orders may be added or filled, notional walks with market prices, and
// pending/failed settlements ebb and flow.

export type Policy = Readonly<{
  maxOpenOrders?: number;
  maxOpenNotional?: string;                   // decimal string
  maxPendingSettlements?: number;
  maxFailedSettlements?: number;
  perMarketMaxNotional: Record<string, string>;
}>;

export type RiskConfig = Readonly<{
  markets: string[];                          // ordered market list
  startingPrices: number[];                   // one per market
  policy: Policy;
  enforce: boolean;
  checkIntervalSecs: number;                  // >= 1
  spawnPerCycle: number;                      // avg number of new orders per cycle
}>;

export type OrderRow = {
  orderId: number;
  market: string;
  side: "BID" | "OFFER";
  price: string;                              // decimal string
  qty: string;                                // decimal string
  createdAt: number;
};

export type PendingRow = {
  proposalId: number;
  market: string;
  status: "PENDING" | "FAILED";
  createdAt: number;
};

export type CheckHit = Readonly<{
  key: string;                                // e.g. "max_open_orders"
  observed: string;                           // observed value stringified
  limit: string;                              // limit value stringified
  enforced: boolean;                          // whether we cancelled to fix
  cancelled: number;                          // orders cancelled to remediate
}>;

export type RiskEvent = Readonly<{
  seq: number;
  t: number;
  openOrders: number;
  openNotional: string;
  perMarketNotional: Record<string, string>;
  pending: number;
  failed: number;
  hits: CheckHit[];
  enforcedCancellations: number;
}>;

export type RiskState = {
  config: RiskConfig;
  status: "running" | "idle";
  cycle: number;
  currentPrices: number[];                    // walk-target per market
  orders: OrderRow[];
  pending: PendingRow[];
  totalCancelled: number;
  totalBreaches: number;
  lastEvent: RiskEvent | null;
  recentEvents: RiskEvent[];                  // bounded ~30
  nextRunAt: number;
  nextOrderId: number;
  nextProposalId: number;
  history: Array<{ t: number; openNotional: number; limit: number | null }>; // for chart
};

const MAX_RECENT = 30;
const MAX_HISTORY = 200;

export function initState(config: RiskConfig, now: number): RiskState {
  const prices = config.markets.map((_, i) => config.startingPrices[i] ?? config.startingPrices[0] ?? 1);
  return {
    config,
    status: "running",
    cycle: 0,
    currentPrices: prices,
    orders: [],
    pending: [],
    totalCancelled: 0,
    totalBreaches: 0,
    lastEvent: null,
    recentEvents: [],
    nextRunAt: now + config.checkIntervalSecs * 1000,
    nextOrderId: 1,
    nextProposalId: 1,
    history: [],
  };
}

/** Advance one tick. `_driverPrice` is used only to influence price walk. */
export function step(state: RiskState, driverPrice: number, now: number): { state: RiskState; log: string[] } {
  if (state.status !== "running") return { state, log: [] };
  const log: string[] = [];

  // Walk each market's price independently around the driver.
  state.currentPrices = state.currentPrices.map((p, i) => {
    const anchor = i === 0 ? driverPrice : p;
    return jitter(anchor === p ? p : anchor, 0.004);
  });

  // Spawn new orders / occasionally decay old ones.
  spawnAndDecay(state, now);

  // Update pending / failed settlements — occasionally flip pending → settled (drop) or failed.
  churnPending(state, now);

  // If the check interval elapsed, run a policy evaluation.
  if (now >= state.nextRunAt) {
    const event = evaluate(state, now, log);
    state.lastEvent = event;
    state.recentEvents.push(event);
    while (state.recentEvents.length > MAX_RECENT) state.recentEvents.shift();
    state.nextRunAt = now + state.config.checkIntervalSecs * 1000;

    // History for chart — track open notional against optional overall cap.
    const cap = state.config.policy.maxOpenNotional ? Number(state.config.policy.maxOpenNotional) : null;
    state.history.push({ t: now, openNotional: Number(event.openNotional), limit: cap });
    while (state.history.length > MAX_HISTORY) state.history.shift();
  }

  return { state, log };
}

function spawnAndDecay(state: RiskState, now: number) {
  const avg = state.config.spawnPerCycle / 4;  // scaled per tick (interval typically > tick)
  const spawn = Math.floor(avg) + (Math.random() < avg - Math.floor(avg) ? 1 : 0);
  for (let i = 0; i < spawn; i++) {
    const mIdx = Math.floor(Math.random() * state.config.markets.length);
    const market = state.config.markets[mIdx];
    const price = state.currentPrices[mIdx] * (1 + (Math.random() - 0.5) * 0.02);
    const qty = Math.round((0.5 + Math.random() * 8) * 100) / 100;
    state.orders.push({
      orderId: state.nextOrderId++,
      market,
      side: Math.random() < 0.5 ? "BID" : "OFFER",
      price: price.toFixed(6),
      qty: qty.toString(),
      createdAt: now,
    });
  }
  // Random ~5% orders fill each tick.
  const survivors: OrderRow[] = [];
  for (const o of state.orders) {
    if (Math.random() < 0.05) {
      // Simulate a fill → maybe a pending proposal.
      if (Math.random() < 0.7) {
        state.pending.push({
          proposalId: state.nextProposalId++,
          market: o.market,
          status: "PENDING",
          createdAt: now,
        });
      }
    } else {
      survivors.push(o);
    }
  }
  state.orders = survivors;
}

function churnPending(state: RiskState, _now: number) {
  const kept: PendingRow[] = [];
  for (const p of state.pending) {
    if (p.status === "PENDING") {
      const r = Math.random();
      if (r < 0.15) continue;                     // settled → remove
      if (r < 0.20) kept.push({ ...p, status: "FAILED" }); // ~5% flip to failed
      else kept.push(p);
    } else {
      // Failed proposals sit around for a while — 10% cleared per tick.
      if (Math.random() > 0.10) kept.push(p);
    }
  }
  state.pending = kept;
}

function evaluate(state: RiskState, now: number, log: string[]): RiskEvent {
  const policy = state.config.policy;
  let openNotional = 0;
  const perMarket: Record<string, number> = {};
  for (const o of state.orders) {
    const n = Number(o.price) * Number(o.qty);
    openNotional += n;
    perMarket[o.market] = (perMarket[o.market] ?? 0) + n;
  }
  const pendingCount = state.pending.filter((p) => p.status === "PENDING").length;
  const failedCount = state.pending.filter((p) => p.status === "FAILED").length;

  const hits: CheckHit[] = [];
  let enforcedCancellations = 0;

  // max_open_orders
  if (typeof policy.maxOpenOrders === "number") {
    const n = state.orders.length;
    if (n > policy.maxOpenOrders) {
      const excess = n - policy.maxOpenOrders;
      const cancelled = state.config.enforce ? cancelNewest(state, excess, log) : 0;
      enforcedCancellations += cancelled;
      hits.push({ key: "max_open_orders", observed: String(n), limit: String(policy.maxOpenOrders), enforced: state.config.enforce, cancelled });
    }
  }

  // max_open_notional
  if (policy.maxOpenNotional) {
    const cap = Number(policy.maxOpenNotional);
    if (openNotional > cap) {
      let cancelled = 0;
      if (state.config.enforce) {
        cancelled = cancelUntilNotionalUnder(state, cap, undefined, log);
        // recompute after cancellation
        openNotional = 0;
        for (const o of state.orders) openNotional += Number(o.price) * Number(o.qty);
      }
      enforcedCancellations += cancelled;
      hits.push({ key: "max_open_notional", observed: openNotional.toFixed(2), limit: cap.toFixed(2), enforced: state.config.enforce, cancelled });
    }
  }

  // per_market_max_notional
  for (const [market, capStr] of Object.entries(policy.perMarketMaxNotional)) {
    const cap = Number(capStr);
    const val = perMarket[market] ?? 0;
    if (val > cap) {
      let cancelled = 0;
      if (state.config.enforce) {
        cancelled = cancelUntilNotionalUnder(state, cap, market, log);
        // recompute perMarket for this market
        let recomputed = 0;
        for (const o of state.orders) if (o.market === market) recomputed += Number(o.price) * Number(o.qty);
        perMarket[market] = recomputed;
      }
      enforcedCancellations += cancelled;
      hits.push({ key: `per_market_max_notional[${market}]`, observed: (perMarket[market] ?? 0).toFixed(2), limit: cap.toFixed(2), enforced: state.config.enforce, cancelled });
    }
  }

  // pending / failed — observe-only per Rust source
  if (typeof policy.maxPendingSettlements === "number" && pendingCount > policy.maxPendingSettlements) {
    hits.push({ key: "max_pending_settlements", observed: String(pendingCount), limit: String(policy.maxPendingSettlements), enforced: false, cancelled: 0 });
  }
  if (typeof policy.maxFailedSettlements === "number" && failedCount > policy.maxFailedSettlements) {
    hits.push({ key: "max_failed_settlements", observed: String(failedCount), limit: String(policy.maxFailedSettlements), enforced: false, cancelled: 0 });
  }

  const seq = state.cycle + 1;
  state.cycle = seq;
  state.totalCancelled += enforcedCancellations;
  if (hits.length > 0) state.totalBreaches += hits.length;

  const event: RiskEvent = {
    seq,
    t: now,
    openOrders: state.orders.length,
    openNotional: openNotional.toFixed(2),
    perMarketNotional: Object.fromEntries(Object.entries(perMarket).map(([k, v]) => [k, v.toFixed(2)])),
    pending: pendingCount,
    failed: failedCount,
    hits,
    enforcedCancellations,
  };

  if (hits.length === 0) {
    log.push(`OK    cycle=${seq}  orders=${state.orders.length}  notional=${openNotional.toFixed(2)}  pending=${pendingCount} failed=${failedCount}`);
  } else {
    log.push(`BREACH cycle=${seq}  hits=${hits.length}  cancelled=${enforcedCancellations}  [${hits.map((h) => h.key).join(",")}]`);
  }
  return event;
}

function cancelNewest(state: RiskState, count: number, log: string[]): number {
  const sorted = [...state.orders].sort((a, b) => b.orderId - a.orderId);
  const victims = sorted.slice(0, count);
  const victimIds = new Set(victims.map((v) => v.orderId));
  state.orders = state.orders.filter((o) => !victimIds.has(o.orderId));
  for (const v of victims) log.push(`CANCEL order_id=${v.orderId} market=${v.market} side=${v.side}`);
  return victims.length;
}

function cancelUntilNotionalUnder(state: RiskState, cap: number, market: string | undefined, log: string[]): number {
  const scope = market ? state.orders.filter((o) => o.market === market) : state.orders.slice();
  const sorted = [...scope].sort((a, b) => b.orderId - a.orderId);
  let shaved = market
    ? scope.reduce((acc, o) => acc + Number(o.price) * Number(o.qty), 0)
    : state.orders.reduce((acc, o) => acc + Number(o.price) * Number(o.qty), 0);
  const victims: OrderRow[] = [];
  for (const o of sorted) {
    if (shaved <= cap) break;
    shaved -= Number(o.price) * Number(o.qty);
    victims.push(o);
  }
  const victimIds = new Set(victims.map((v) => v.orderId));
  state.orders = state.orders.filter((o) => !victimIds.has(o.orderId));
  for (const v of victims) log.push(`CANCEL order_id=${v.orderId} market=${v.market} side=${v.side} (notional shave)`);
  return victims.length;
}

function jitter(current: number, vol: number): number {
  const u1 = Math.max(1e-9, Math.random());
  const u2 = Math.random();
  const z = Math.sqrt(-2 * Math.log(u1)) * Math.cos(2 * Math.PI * u2);
  const next = current * (1 + vol * z);
  return next > 0 ? next : current * 0.5;
}
