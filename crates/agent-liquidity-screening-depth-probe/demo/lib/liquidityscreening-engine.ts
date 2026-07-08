// Port of agent-liquidity-screening-depth-probe snapshot logic to TypeScript. Mirrors
// crates/agent-liquidity-screening-depth-probe/src/main.rs (fn sample_market + fn walk).
//
// Each poll cycle simulates a synthetic orderbook around the current mid,
// then computes:
//   - spread / spread_bps
//   - bid_depth_total / offer_depth_total
//   - probe VWAP for a market-BUY of probeQty against offers
//   - probe VWAP for a market-SELL of probeQty against bids
//   - slippage_bps = |vwap - mid| / mid * 10000

export type Level = Readonly<{ price: number; qty: number }>;

export type LiquidityScreeningConfig = Readonly<{
  market: string;
  probeQty: number;         // qty used to probe VWAP slippage
  depth: number;            // number of levels per side
  pollSecs: number;         // publish cadence (secs)
  startingPrice: number;    // seed mid
  spreadBps: number;        // baseline spread (bps) between best_bid and best_offer
  depthFalloffPct: number;  // spacing between adjacent levels in percent
}>;

export type LiquidityScreeningState = {
  config: LiquidityScreeningConfig;
  status: "monitoring" | "idle";
  currentPrice: number;
  bidLevels: Level[];
  offerLevels: Level[];
  spread: number;
  spreadBps: number;
  bidDepthTotal: number;
  offerDepthTotal: number;
  bidNotionalTotal: number;
  offerNotionalTotal: number;
  buyProbeVwap: number | null;
  sellProbeVwap: number | null;
  buyProbeSlippageBps: number | null;
  sellProbeSlippageBps: number | null;
  buyProbeFilled: number;
  sellProbeFilled: number;
  buyProbeLevelsUsed: number;
  sellProbeLevelsUsed: number;
  snapshotsCount: number;
  lastSnapshotAt: number | null;
  ticksSinceLastSnapshot: number;
};

export function initState(config: LiquidityScreeningConfig, startPrice: number): LiquidityScreeningState {
  const s: LiquidityScreeningState = {
    config,
    status: "monitoring",
    currentPrice: startPrice,
    bidLevels: [],
    offerLevels: [],
    spread: 0,
    spreadBps: 0,
    bidDepthTotal: 0,
    offerDepthTotal: 0,
    bidNotionalTotal: 0,
    offerNotionalTotal: 0,
    buyProbeVwap: null,
    sellProbeVwap: null,
    buyProbeSlippageBps: null,
    sellProbeSlippageBps: null,
    buyProbeFilled: 0,
    sellProbeFilled: 0,
    buyProbeLevelsUsed: 0,
    sellProbeLevelsUsed: 0,
    snapshotsCount: 0,
    lastSnapshotAt: null,
    ticksSinceLastSnapshot: 0,
  };
  rebuildBook(s, startPrice);
  return s;
}

/** Simulate the synthetic book around `price` with a random qty perturbation. */
function rebuildBook(state: LiquidityScreeningState, price: number): void {
  const { depth, spreadBps, depthFalloffPct } = state.config;
  const halfSpread = spreadBps / 2 / 10000; // fractional
  const step = depthFalloffPct / 100;
  const bids: Level[] = [];
  const offers: Level[] = [];
  for (let i = 0; i < depth; i++) {
    const bidPx = price * (1 - halfSpread - i * step);
    const offerPx = price * (1 + halfSpread + i * step);
    // Base qty rises deeper in the book; add ±20% random noise so depth "breathes".
    const baseQty = 5 * (1 + i * 0.5);
    const bidQty = round4(baseQty * (0.8 + Math.random() * 0.4));
    const offerQty = round4(baseQty * (0.8 + Math.random() * 0.4));
    bids.push({ price: round8(bidPx), qty: bidQty });
    offers.push({ price: round8(offerPx), qty: offerQty });
  }
  state.bidLevels = bids;
  state.offerLevels = offers;
}

/** Walks `levels` from best inward, filling `target`. Returns VWAP + slippage_bps vs mid. */
export function walkProbe(
  levels: readonly Level[],
  target: number,
  mid: number,
): { vwap: number | null; filled: number; levelsUsed: number; slippageBps: number | null } {
  let remaining = target;
  let cost = 0;
  let filled = 0;
  let used = 0;
  for (const l of levels) {
    if (l.qty <= 0) continue;
    used += 1;
    if (remaining <= l.qty) {
      cost += remaining * l.price;
      filled += remaining;
      remaining = 0;
      break;
    }
    cost += l.qty * l.price;
    filled += l.qty;
    remaining -= l.qty;
  }
  const vwap = filled > 0 ? cost / filled : null;
  const slippageBps = vwap !== null && mid > 0 ? (Math.abs(vwap - mid) / mid) * 10000 : null;
  return { vwap, filled, levelsUsed: used, slippageBps };
}

/** Applies a tick. Rebuilds book from mid, recomputes metrics. Emits a snapshot line every pollSecs. */
export function step(
  state: LiquidityScreeningState,
  price: number,
  now: number,
): { state: LiquidityScreeningState; events: string[] } {
  if (state.status !== "monitoring" || price <= 0) return { state, events: [] };
  const events: string[] = [];
  state.currentPrice = price;
  rebuildBook(state, price);

  const bestBid = state.bidLevels[0]?.price ?? 0;
  const bestOffer = state.offerLevels[0]?.price ?? 0;
  state.spread = bestOffer - bestBid;
  state.spreadBps = price > 0 ? (state.spread / price) * 10000 : 0;

  let bidQ = 0, bidN = 0;
  for (const l of state.bidLevels) { bidQ += l.qty; bidN += l.qty * l.price; }
  let offerQ = 0, offerN = 0;
  for (const l of state.offerLevels) { offerQ += l.qty; offerN += l.qty * l.price; }
  state.bidDepthTotal = bidQ;
  state.offerDepthTotal = offerQ;
  state.bidNotionalTotal = bidN;
  state.offerNotionalTotal = offerN;

  const mid = (bestBid + bestOffer) / 2;
  const buyP = walkProbe(state.offerLevels, state.config.probeQty, mid);
  const sellP = walkProbe(state.bidLevels, state.config.probeQty, mid);
  state.buyProbeVwap = buyP.vwap;
  state.buyProbeFilled = buyP.filled;
  state.buyProbeLevelsUsed = buyP.levelsUsed;
  state.buyProbeSlippageBps = buyP.slippageBps;
  state.sellProbeVwap = sellP.vwap;
  state.sellProbeFilled = sellP.filled;
  state.sellProbeLevelsUsed = sellP.levelsUsed;
  state.sellProbeSlippageBps = sellP.slippageBps;

  state.ticksSinceLastSnapshot += 1;
  if (state.ticksSinceLastSnapshot >= state.config.pollSecs) {
    state.ticksSinceLastSnapshot = 0;
    state.snapshotsCount += 1;
    state.lastSnapshotAt = now;
    events.push(
      `SNAPSHOT #${state.snapshotsCount} ${state.config.market} mid=${fmt(mid)} spread=${fmt(state.spread)} (${state.spreadBps.toFixed(2)} bps) bid_depth=${fmt(bidQ)} offer_depth=${fmt(offerQ)}`,
    );
    if (buyP.vwap !== null && buyP.slippageBps !== null) {
      events.push(
        `PROBE BUY  qty=${state.config.probeQty} vwap=${fmt(buyP.vwap)} slip=+${buyP.slippageBps.toFixed(2)} bps (levels used: ${buyP.levelsUsed})`,
      );
    }
    if (sellP.vwap !== null && sellP.slippageBps !== null) {
      events.push(
        `PROBE SELL qty=${state.config.probeQty} vwap=${fmt(sellP.vwap)} slip=-${sellP.slippageBps.toFixed(2)} bps (levels used: ${sellP.levelsUsed})`,
      );
    }
  }

  return { state, events };
}

function round4(n: number): number { return Math.round(n * 1e4) / 1e4; }
function round8(n: number): number { return Math.round(n * 1e8) / 1e8; }
function fmt(n: number): string {
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? 4 : 6;
  return n.toFixed(digits);
}
