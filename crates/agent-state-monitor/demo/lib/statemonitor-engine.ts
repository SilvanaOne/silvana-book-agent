// Port of agent-state-monitor observer logic to TypeScript. Mirrors
// crates/agent-state-monitor/src/main.rs.
//
// This is a read-only observer. It subscribes to SubscribeOrders +
// SubscribeSettlements streams and logs every event (created/filled/cancelled,
// proposal/settled/failed). The demo synthesizes those event streams via a
// simple Poisson arrival process on top of a GBM mid.

export type OrderKind = "created" | "filled" | "cancelled";
export type SettlementKind = "proposal" | "settled" | "failed";
export type OrderSide = "BID" | "OFFER";

export type StateMonitorConfig = Readonly<{
  market: string;                     // optional filter; "" or "*" = all markets
  includeOrders: boolean;
  includeSettlements: boolean;
  orderArrivalPerTick: number;        // 0..1 probability of any order event per tick
  settlementArrivalPerTick: number;   // 0..1 probability of any settlement event per tick
  startingPrice: number;
}>;

export type OrderEvent = Readonly<{
  seq: number;
  t: number;
  market: string;
  side: OrderSide;
  price: number;
  qty: number;
  kind: OrderKind;
  orderId: string;
}>;

export type SettlementEvent = Readonly<{
  seq: number;
  t: number;
  kind: SettlementKind;
  proposalId: string;
  notional: number;
  market: string;
}>;

export type StateMonitorStats = {
  ordersCreated: number;
  ordersFilled: number;
  ordersCancelled: number;
  settlementsProposal: number;
  settlementsSettled: number;
  settlementsFailed: number;
};

// Internal open-order bookkeeping so we can pick a "next event target" and
// synthesize plausible follow-ups (fill or cancel) for previously-created ids.
type OpenOrder = {
  orderId: string;
  seq: number;
  market: string;
  side: OrderSide;
  price: number;
  qty: number;
  createdAt: number;
};

type PendingProposal = {
  proposalId: string;
  seq: number;
  market: string;
  notional: number;
  createdAt: number;
};

export type StateMonitorState = {
  config: StateMonitorConfig;
  status: "monitoring" | "idle";
  currentPrice: number;
  ticks: number;
  startedAt: number;
  orderEvents: OrderEvent[];        // bounded ~40
  settlementEvents: SettlementEvent[]; // bounded ~40
  stats: StateMonitorStats;

  // Bookkeeping (not surfaced in InfoGrid but drives realistic follow-ups)
  openOrders: OpenOrder[];
  pendingProposals: PendingProposal[];
  orderSeq: number;
  settlementSeq: number;
};

const MAX_EVENTS = 40;
const MAX_OPEN_ORDERS = 20;
const MAX_PENDING_PROPOSALS = 20;

const MARKET_POOL = ["CC-USDC", "BTC-USD", "ETH-USD", "SOL-USDC"];

export function initState(config: StateMonitorConfig, startPrice: number): StateMonitorState {
  return {
    config,
    status: "monitoring",
    currentPrice: startPrice,
    ticks: 0,
    startedAt: Date.now(),
    orderEvents: [],
    settlementEvents: [],
    stats: {
      ordersCreated: 0,
      ordersFilled: 0,
      ordersCancelled: 0,
      settlementsProposal: 0,
      settlementsSettled: 0,
      settlementsFailed: 0,
    },
    openOrders: [],
    pendingProposals: [],
    orderSeq: 0,
    settlementSeq: 0,
  };
}

/** Applies a tick. Returns state + emitted log lines. */
export function step(state: StateMonitorState, price: number, now: number): { state: StateMonitorState; events: string[] } {
  if (state.status !== "monitoring" || price <= 0) return { state, events: [] };
  const events: string[] = [];
  state.currentPrice = price;
  state.ticks += 1;

  const filter = normFilter(state.config.market);

  // ────── Order stream ──────
  if (state.config.includeOrders) {
    if (Math.random() < clampProb(state.config.orderArrivalPerTick)) {
      const roll = Math.random();
      // 60% new "created", 25% fill of an existing open order, 15% cancel
      if (roll < 0.60 || state.openOrders.length === 0) {
        const market = pickMarket(filter);
        const side: OrderSide = Math.random() < 0.5 ? "BID" : "OFFER";
        const drift = 1 + (Math.random() - 0.5) * 0.02; // ±1%
        const evPrice = round8(price * drift);
        const qty = round4(0.5 + Math.random() * 4.5);
        state.orderSeq += 1;
        const orderId = `o-${randHex(6)}`;
        const created: OrderEvent = {
          seq: state.orderSeq, t: now, market, side, price: evPrice, qty, kind: "created", orderId,
        };
        if (matchesFilter(filter, market)) {
          pushEvent(state.orderEvents, created);
          state.stats.ordersCreated += 1;
          state.openOrders.push({ orderId, seq: state.orderSeq, market, side, price: evPrice, qty, createdAt: now });
          if (state.openOrders.length > MAX_OPEN_ORDERS) state.openOrders.shift();
          events.push(`ORDER #${created.seq} created ${market} ${side} ${fmtQty(qty)}@${fmt(evPrice)} id=${orderId}`);
        }
      } else {
        // Consume an open order → fill or cancel
        const idx = Math.floor(Math.random() * state.openOrders.length);
        const o = state.openOrders[idx];
        state.openOrders.splice(idx, 1);
        const kind: OrderKind = roll < 0.85 ? "filled" : "cancelled";
        state.orderSeq += 1;
        const ev: OrderEvent = {
          seq: state.orderSeq, t: now, market: o.market, side: o.side, price: o.price, qty: o.qty, kind, orderId: o.orderId,
        };
        if (matchesFilter(filter, o.market)) {
          pushEvent(state.orderEvents, ev);
          if (kind === "filled") state.stats.ordersFilled += 1;
          else state.stats.ordersCancelled += 1;
          events.push(`ORDER #${ev.seq} ${kind} ${o.market} ${o.side} ${fmtQty(o.qty)}@${fmt(o.price)} id=${o.orderId}`);
        }
      }
    }
  }

  // ────── Settlement stream ──────
  if (state.config.includeSettlements) {
    if (Math.random() < clampProb(state.config.settlementArrivalPerTick)) {
      const roll = Math.random();
      // 55% new proposal, 35% settled, 10% failed (both consume a pending one)
      if (roll < 0.55 || state.pendingProposals.length === 0) {
        const market = pickMarket(filter);
        const notional = round4((0.5 + Math.random() * 8) * price);
        state.settlementSeq += 1;
        const proposalId = `p-${randHex(6)}`;
        const ev: SettlementEvent = {
          seq: state.settlementSeq, t: now, kind: "proposal", proposalId, notional, market,
        };
        if (matchesFilter(filter, market)) {
          pushEvent(state.settlementEvents, ev);
          state.stats.settlementsProposal += 1;
          state.pendingProposals.push({ proposalId, seq: state.settlementSeq, market, notional, createdAt: now });
          if (state.pendingProposals.length > MAX_PENDING_PROPOSALS) state.pendingProposals.shift();
          events.push(`SETTLEMENT #${ev.seq} proposal ${market} notional=${fmt(notional)} id=${proposalId}`);
        }
      } else {
        const idx = Math.floor(Math.random() * state.pendingProposals.length);
        const p = state.pendingProposals[idx];
        state.pendingProposals.splice(idx, 1);
        const kind: SettlementKind = roll < 0.90 ? "settled" : "failed";
        state.settlementSeq += 1;
        const ev: SettlementEvent = {
          seq: state.settlementSeq, t: now, kind, proposalId: p.proposalId, notional: p.notional, market: p.market,
        };
        if (matchesFilter(filter, p.market)) {
          pushEvent(state.settlementEvents, ev);
          if (kind === "settled") state.stats.settlementsSettled += 1;
          else state.stats.settlementsFailed += 1;
          events.push(`SETTLEMENT #${ev.seq} ${kind} ${p.market} notional=${fmt(p.notional)} id=${p.proposalId}`);
        }
      }
    }
  }

  return { state, events };
}

function pushEvent<T>(arr: T[], ev: T) {
  arr.push(ev);
  while (arr.length > MAX_EVENTS) arr.shift();
}

function normFilter(m: string): string {
  const s = (m ?? "").trim();
  if (!s || s === "*") return "";
  return s;
}

function matchesFilter(filter: string, market: string): boolean {
  if (!filter) return true;
  return filter === market;
}

function pickMarket(filter: string): string {
  if (filter) return filter;
  return MARKET_POOL[Math.floor(Math.random() * MARKET_POOL.length)];
}

function clampProb(p: number): number {
  if (!Number.isFinite(p) || p < 0) return 0;
  if (p > 1) return 1;
  return p;
}

function randHex(n: number): string {
  let s = "";
  for (let i = 0; i < n; i++) s += Math.floor(Math.random() * 16).toString(16);
  return s;
}

function round8(n: number): number { return Math.round(n * 1e8) / 1e8; }
function round4(n: number): number { return Math.round(n * 1e4) / 1e4; }

function fmt(n: number): string {
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? 4 : 6;
  return n.toFixed(digits);
}

function fmtQty(n: number): string {
  const abs = Math.abs(n);
  const digits = abs >= 10 ? 2 : 4;
  return n.toFixed(digits);
}
