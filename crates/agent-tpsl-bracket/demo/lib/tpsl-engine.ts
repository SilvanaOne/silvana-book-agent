// Port of agent-tpsl-bracket monitoring logic to TypeScript. Mirrors
// crates/agent-tpsl-bracket/src/main.rs (fn bracket_loop). Both TP and SL
// are placed as resting limit orders at once (OCO); the engine tracks
// which side the mid crosses first, marks it "filled", and cancels the
// sibling.

export type Side = "long" | "short";

export type PositionConfig = Readonly<{
  market: string;
  side: Side;
  quantity: number;
  entryPrice: number;
  tp: number;   // required in bracket mode
  sl: number;   // required in bracket mode
}>;

export type BracketLeg = {
  price: number;
  status: "resting" | "filled" | "cancelled";
  filledAt?: number;
};

export type PositionState = {
  config: PositionConfig;
  status: "idle" | "monitoring" | "triggered";
  currentPrice: number;
  tpLeg: BracketLeg;
  slLeg: BracketLeg;
  triggeredReason: null | "TAKE PROFIT" | "STOP LOSS";
  triggeredAt: number | null;
  pnl: number;
};

export function initState(config: PositionConfig, startPrice: number): PositionState {
  return {
    config,
    status: "monitoring",
    currentPrice: startPrice,
    tpLeg: { price: config.tp, status: "resting" },
    slLeg: { price: config.sl, status: "resting" },
    triggeredReason: null,
    triggeredAt: null,
    pnl: 0,
  };
}

/**
 * Applies a new market price. When mid crosses one bracket leg, that leg
 * is marked filled; the sibling is cancelled and the position is closed.
 */
export function step(state: PositionState, price: number, now: number): { state: PositionState; events: string[] } {
  if (state.status === "triggered") return { state, events: [] };
  if (price <= 0) return { state, events: [] };

  const events: string[] = [];
  state.currentPrice = price;
  state.pnl = computePnl(state.config, price);

  const side = state.config.side;

  // Trigger rules — long TP fills on price >= tp, SL on price <= sl;
  // short is symmetric.
  let hit: "TP" | "SL" | null = null;
  if (side === "long") {
    if (price >= state.tpLeg.price) hit = "TP";
    else if (price <= state.slLeg.price) hit = "SL";
  } else {
    if (price <= state.tpLeg.price) hit = "TP";
    else if (price >= state.slLeg.price) hit = "SL";
  }

  if (hit === null) return { state, events };

  const dir = side === "long" ? "SELL" : "BUY";
  if (hit === "TP") {
    state.tpLeg.status = "filled";
    state.tpLeg.filledAt = now;
    state.slLeg.status = "cancelled";
    state.triggeredReason = "TAKE PROFIT";
    events.push(`🎯 TP filled — ${dir} ${state.config.quantity} ${state.config.market} @ ${fmt(state.tpLeg.price)} · cancelling SL @ ${fmt(state.slLeg.price)}`);
  } else {
    state.slLeg.status = "filled";
    state.slLeg.filledAt = now;
    state.tpLeg.status = "cancelled";
    state.triggeredReason = "STOP LOSS";
    events.push(`🛑 SL filled — ${dir} ${state.config.quantity} ${state.config.market} @ ${fmt(state.slLeg.price)} · cancelling TP @ ${fmt(state.tpLeg.price)}`);
  }
  state.status = "triggered";
  state.triggeredAt = now;
  const legPrice = hit === "TP" ? state.tpLeg.price : state.slLeg.price;
  events.push(`Exit realized @ ${fmt(legPrice)}, PnL ${state.pnl >= 0 ? "+" : ""}${fmt(state.pnl)}`);
  return { state, events };
}

export function computePnl(config: PositionConfig, price: number): number {
  const delta = config.side === "long" ? price - config.entryPrice : config.entryPrice - price;
  return delta * config.quantity;
}

function fmt(n: number | null): string {
  if (n === null) return "—";
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? 4 : 8;
  return n.toFixed(digits);
}
