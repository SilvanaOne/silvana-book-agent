// Port of agent-pnl-screening-settlement-stream logic to TypeScript. Mirrors
// crates/agent-pnl-screening-settlement-stream/src/main.rs.
//
// The real agent subscribes to the settlement stream and accumulates
// per-market position, weighted-average cost basis, realized PnL and
// live unrealized PnL. This demo simulates settlement events on a
// synthetic price walk and produces the same snapshot shape.

export type Side = "BUY" | "SELL";

export type PnlScreeningConfig = Readonly<{
  market: string;
  snapshotIntervalSecs: number;
  tradeArrivalPerTick: number; // Poisson-ish arrival rate per 1s tick
  avgTradeQty: number;
  startingPrice: number;
  startingPosition: number;
  startingCostBasis: number;
}>;

export type PnlTrade = Readonly<{
  seq: number;
  t: number;
  side: Side;
  qty: number;
  price: number;
  realizedDelta: number;
  positionAfter: number;
  costBasisAfter: number;
}>;

export type PnlScreeningState = {
  config: PnlScreeningConfig;
  status: "monitoring" | "idle";
  currentPrice: number;
  position: number;
  avgCostBasis: number;
  realizedPnl: number;
  unrealizedPnl: number;
  tradesCount: number;
  buysCount: number;
  sellsCount: number;
  snapshotsCount: number;
  lastSnapshotAt: number | null;
  startedAt: number;
  trades: PnlTrade[];
};

const MAX_TRADES = 40;

export function initState(config: PnlScreeningConfig, now: number): PnlScreeningState {
  return {
    config,
    status: "monitoring",
    currentPrice: config.startingPrice,
    position: config.startingPosition,
    avgCostBasis: config.startingPosition > 0 ? config.startingCostBasis : 0,
    realizedPnl: 0,
    unrealizedPnl: 0,
    tradesCount: 0,
    buysCount: 0,
    sellsCount: 0,
    snapshotsCount: 0,
    lastSnapshotAt: null,
    startedAt: now,
    trades: [],
  };
}

/** Poisson-ish trade arrival: at most 1 trade per tick when rate<=1, otherwise multiple. */
function drawTradeCount(rate: number): number {
  // For small rates use Bernoulli; for larger use naive iteration.
  if (rate <= 0) return 0;
  if (rate <= 1) return Math.random() < rate ? 1 : 0;
  let n = 0;
  let r = rate;
  while (r > 0) {
    if (r >= 1) {
      n += 1;
      r -= 1;
    } else {
      if (Math.random() < r) n += 1;
      break;
    }
  }
  return n;
}

/** Apply one tick: update price, maybe generate one or more synthetic trades, and emit an interval snapshot if due. */
export function step(state: PnlScreeningState, price: number, now: number): { state: PnlScreeningState; events: string[] } {
  if (state.status !== "monitoring" || price <= 0) return { state, events: [] };
  const events: string[] = [];
  state.currentPrice = price;

  const nTrades = drawTradeCount(state.config.tradeArrivalPerTick);
  for (let i = 0; i < nTrades; i++) {
    const side: Side = Math.random() < 0.5 ? "BUY" : "SELL";
    const qtyRaw = state.config.avgTradeQty * (0.5 + Math.random());
    const qty = Math.max(round8(qtyRaw), 0.00000001);

    const prevPos = state.position;
    const prevCost = state.avgCostBasis;
    let realizedDelta = 0;
    let newPos = prevPos;
    let newCost = prevCost;

    if (side === "BUY") {
      const nextPos = prevPos + qty;
      newCost = nextPos > 0 ? (prevPos * prevCost + qty * price) / nextPos : 0;
      newPos = nextPos;
    } else {
      const closed = Math.min(qty, Math.max(prevPos, 0));
      if (closed > 0) {
        realizedDelta = (price - prevCost) * closed;
      }
      newPos = Math.max(prevPos - qty, 0);
      if (newPos === 0) newCost = 0;
      // else keep prevCost (WAVG unchanged on partial sells)
    }

    state.position = round8(newPos);
    state.avgCostBasis = round8(newCost);
    state.realizedPnl += realizedDelta;
    state.tradesCount += 1;
    if (side === "BUY") state.buysCount += 1; else state.sellsCount += 1;

    const seq = state.tradesCount;
    const trade: PnlTrade = {
      seq,
      t: now,
      side,
      qty,
      price,
      realizedDelta,
      positionAfter: state.position,
      costBasisAfter: state.avgCostBasis,
    };
    state.trades.push(trade);
    if (state.trades.length > MAX_TRADES) state.trades.shift();

    events.push(
      `FILL #${seq} ${side} ${fmt(qty)} @ ${fmt(price)} pos=${fmt(state.position)} cost=${fmt(state.avgCostBasis)} realΔ=${sgn(realizedDelta)}${fmt(Math.abs(realizedDelta))}`,
    );
  }

  // Live unrealized (long-only in this demo)
  state.unrealizedPnl = state.position > 0 ? state.position * (price - state.avgCostBasis) : 0;

  // Emit periodic snapshot
  const dueMs = state.config.snapshotIntervalSecs * 1000;
  if (state.lastSnapshotAt === null || now - state.lastSnapshotAt >= dueMs) {
    state.lastSnapshotAt = now;
    state.snapshotsCount += 1;
    events.push(
      `SNAPSHOT #${state.snapshotsCount}: pos=${fmt(state.position)} cost=${fmt(state.avgCostBasis)} real=${sgn(state.realizedPnl)}${fmt(Math.abs(state.realizedPnl))} unreal=${sgn(state.unrealizedPnl)}${fmt(Math.abs(state.unrealizedPnl))}`,
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
  return n >= 0 ? "+" : "-";
}
