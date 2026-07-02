// Port of agent-liquidity-seeking execution logic to TypeScript. Mirrors the
// core rule in crates/agent-liquidity-seeking/src/main.rs (fn seek_loop /
// max_safe_qty):
//
//   Each cycle:
//     1. Snapshot the synthetic depth around the live mid.
//     2. Walk the relevant side (buy → offers, sell → bids) accumulating
//        qty until VWAP-vs-mid slippage exceeds `maxSlippageBps`, or until
//        we run out of levels.
//     3. Place a child order of `min(safeQty, maxChunk, remaining)` at
//        the last accepted price boundary.
//     4. Wait for that child to clear (fill or cancel) before probing again.

export type Side = "buy" | "sell";
export type OrderType = "BID" | "OFFER";
export type ChildStatus = "open" | "filled" | "cancelled";

export type Level = Readonly<{ price: number; qty: number }>;
export type Book = Readonly<{ bids: Level[]; offers: Level[] }>;

export type Child = {
  seq: number;
  t: number;
  side: OrderType;
  qty: number;
  price: number;      // VWAP boundary at placement
  vwap: number;
  slippageBps: number;
  status: ChildStatus;
  filledAt?: number;
  filledPrice?: number;
};

export type LiquiditySeekingConfig = Readonly<{
  market: string;
  side: Side;
  total: number;
  maxSlippageBps: number;    // e.g. 25
  maxChunk: number;          // e.g. 5
  depth: number;             // number of levels per side
  maxRuntimeSecs?: number;   // hard timeout for the parent
  startingPrice: number;
  spreadBps: number;         // synthetic book spread, half above/below mid
  depthFalloffPct: number;   // % gap between adjacent levels
}>;

export type LiquiditySeekingState = {
  config: LiquiditySeekingConfig;
  status: "monitoring" | "completed" | "idle";
  currentPrice: number;
  book: Book;
  totalFilled: number;
  currentChild: Child | null;
  childrenPlaced: number;
  childrenFilled: number;
  children: Child[];   // bounded (recent ~30)
  startedAt: number;
  completedAt?: number;
};

const MAX_CHILDREN = 30;

export function initState(config: LiquiditySeekingConfig, startPrice: number, now: number): LiquiditySeekingState {
  return {
    config,
    status: "monitoring",
    currentPrice: startPrice,
    book: buildBook(startPrice, config),
    totalFilled: 0,
    currentChild: null,
    childrenPlaced: 0,
    childrenFilled: 0,
    children: [],
    startedAt: now,
  };
}

/** One simulation tick. */
export function step(state: LiquiditySeekingState, price: number, now: number): { state: LiquiditySeekingState; events: string[] } {
  if (state.status !== "monitoring" || price <= 0) return { state, events: [] };
  const events: string[] = [];
  state.currentPrice = price;
  state.book = buildBook(price, state.config);

  // 1. Fill detection on the currently-open child.
  if (state.currentChild && state.currentChild.status === "open") {
    const c = state.currentChild;
    // BID fills when mid drops to (or below) our BID price; OFFER fills when
    // mid rises to (or above) our OFFER price. This is a stylised sim.
    const filled = c.side === "BID" ? price <= c.price : price >= c.price;
    if (filled) {
      c.status = "filled";
      c.filledAt = now;
      c.filledPrice = price;
      state.totalFilled = round8(state.totalFilled + c.qty);
      state.childrenFilled += 1;
      state.currentChild = null;
      events.push(
        `FILL #${c.seq} ${c.side} ${fmt(c.qty)} @ ${fmt(c.price)} (mid=${fmt(price)}, parent ${fmt(state.totalFilled)}/${fmt(state.config.total)})`,
      );
    }
  }

  // 2. Parent-level completion / runtime guard.
  if (state.totalFilled >= state.config.total) {
    state.status = "completed";
    state.completedAt = now;
    events.push(`PARENT COMPLETE: filled=${fmt(state.totalFilled)}/${fmt(state.config.total)}`);
    return { state, events };
  }
  const runtimeExceeded =
    typeof state.config.maxRuntimeSecs === "number" &&
    now - state.startedAt > state.config.maxRuntimeSecs * 1000;
  if (runtimeExceeded) {
    // Cancel any resting child, mark as idle (not completed) to reflect timeout.
    if (state.currentChild && state.currentChild.status === "open") {
      state.currentChild.status = "cancelled";
      state.currentChild = null;
    }
    state.status = "idle";
    state.completedAt = now;
    events.push(`TIMEOUT: filled=${fmt(state.totalFilled)}/${fmt(state.config.total)} maxRuntime reached`);
    return { state, events };
  }

  // 3. Probe depth and place a child if we don't have one working.
  if (state.currentChild === null) {
    const remaining = state.config.total - state.totalFilled;
    if (remaining <= 0) return { state, events };

    const side: OrderType = state.config.side === "buy" ? "BID" : "OFFER";
    const levels = side === "BID" ? state.book.offers : state.book.bids;
    const mid = midOf(state.book) ?? price;
    const walk = maxSafeQty(levels, mid, state.config.maxSlippageBps, side);
    if (walk.qty <= 0) {
      events.push(`PROBE: no usable depth (mid=${fmt(mid)}, examined=${walk.levelsExamined})`);
      return { state, events };
    }
    const childQty = round8(Math.min(walk.qty, state.config.maxChunk, remaining));
    if (childQty <= 0) return { state, events };
    const seq = state.childrenPlaced + 1;
    const child: Child = {
      seq,
      t: now,
      side,
      qty: childQty,
      price: round8(walk.boundaryPrice),
      vwap: round8(walk.vwap),
      slippageBps: round2(walk.slippageBps),
      status: "open",
    };
    state.currentChild = child;
    state.children.push(child);
    if (state.children.length > MAX_CHILDREN) state.children.shift();
    state.childrenPlaced = seq;
    events.push(
      `PROBE #${seq} ${side}: childQty=${fmt(childQty)} vwap=${fmt(walk.vwap)} slip=${round2(walk.slippageBps)}bps (safe=${fmt(walk.qty)}, remaining=${fmt(remaining)})`,
    );
  }

  return { state, events };
}

/** Build a synthetic orderbook around a mid price. */
export function buildBook(mid: number, cfg: LiquiditySeekingConfig): Book {
  const halfSpread = cfg.spreadBps / 2 / 10000;
  const falloff = cfg.depthFalloffPct / 100;
  const bids: Level[] = [];
  const offers: Level[] = [];
  for (let i = 0; i < cfg.depth; i++) {
    const bidPx = mid * (1 - halfSpread - i * falloff);
    const offerPx = mid * (1 + halfSpread + i * falloff);
    const qty = round8(5 * (1 + i * 0.5));
    if (bidPx > 0) bids.push({ price: round8(bidPx), qty });
    if (offerPx > 0) offers.push({ price: round8(offerPx), qty });
  }
  return { bids, offers };
}

function midOf(b: Book): number | null {
  const bb = b.bids[0]?.price;
  const bo = b.offers[0]?.price;
  if (typeof bb === "number" && typeof bo === "number") return (bb + bo) / 2;
  return null;
}

type WalkResult = { qty: number; boundaryPrice: number; vwap: number; slippageBps: number; levelsExamined: number };

/**
 * Walk levels accumulating qty while VWAP-vs-mid stays within maxBps.
 * Direction:
 *   BID (we're buying) walks offers upward → vwap - mid.
 *   OFFER (we're selling) walks bids downward → mid - vwap.
 */
export function maxSafeQty(levels: readonly Level[], mid: number, maxBps: number, side: OrderType): WalkResult {
  let accumQty = 0;
  let accumCost = 0;
  let lastPrice = mid;
  let acceptedVwap = mid;
  let acceptedBps = 0;
  let examined = 0;
  for (const l of levels) {
    examined += 1;
    if (l.qty <= 0 || l.price <= 0) continue;
    const candQty = accumQty + l.qty;
    const candCost = accumCost + l.qty * l.price;
    const vwap = candCost / candQty;
    const dev = side === "BID" ? vwap - mid : mid - vwap;
    const bps = Math.abs(dev) / mid * 10000;
    if (bps > maxBps) break;
    accumQty = candQty;
    accumCost = candCost;
    lastPrice = l.price;
    acceptedVwap = vwap;
    acceptedBps = bps;
  }
  return {
    qty: accumQty,
    boundaryPrice: lastPrice,
    vwap: acceptedVwap,
    slippageBps: acceptedBps,
    levelsExamined: examined,
  };
}

function round8(n: number): number { return Math.round(n * 1e8) / 1e8; }
function round2(n: number): number { return Math.round(n * 100) / 100; }

function fmt(n: number): string {
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? 4 : 6;
  return n.toFixed(digits);
}
