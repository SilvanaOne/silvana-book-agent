// Port of agent-tpsl monitoring logic to TypeScript. Mirrors
// crates/agent-tpsl/src/main.rs (fn tpsl_loop). Keep in sync with the Rust
// side if trigger semantics change.

export type Side = "long" | "short";

export type PositionConfig = Readonly<{
  market: string;
  side: Side;
  quantity: number;
  entryPrice: number;
  tp: number | null;
  sl: number | null;
  trailingPct: number | null; // e.g. 2.0 = 2%
}>;

export type PositionState = {
  config: PositionConfig;
  status: "idle" | "monitoring" | "triggered";
  currentPrice: number;
  peakPrice: number; // highest for long, lowest for short
  currentSl: number | null; // may move under trailing rules
  triggeredReason: null | "TAKE PROFIT" | "STOP LOSS";
  triggeredAt: number | null; // epoch ms
  pnl: number; // realized-if-exit-now
};

export function initState(config: PositionConfig, startPrice: number): PositionState {
  return {
    config,
    status: "monitoring",
    currentPrice: startPrice,
    peakPrice: startPrice,
    currentSl: config.sl,
    triggeredReason: null,
    triggeredAt: null,
    pnl: 0,
  };
}

/**
 * Applies a new market price to the state. Returns the (possibly-mutated)
 * state and a list of event descriptions raised on this tick.
 */
export function step(state: PositionState, price: number, now: number): { state: PositionState; events: string[] } {
  if (state.status === "triggered") return { state, events: [] };
  if (price <= 0) return { state, events: [] };

  const events: string[] = [];
  const side = state.config.side;

  // --- 1. Trailing stop update (peak / trough tracking) -------------------
  if (state.config.trailingPct !== null && state.config.trailingPct > 0) {
    const trailFactor = state.config.trailingPct / 100.0;

    if (side === "long") {
      if (price > state.peakPrice) state.peakPrice = price;
      const trailing = state.peakPrice * (1 - trailFactor);
      const newSl = state.currentSl === null ? trailing : Math.max(state.currentSl, trailing);
      if (state.currentSl === null || newSl > state.currentSl) {
        events.push(`Trailing SL updated: ${fmt(state.currentSl)} → ${fmt(newSl)} (peak=${fmt(state.peakPrice)})`);
      }
      state.currentSl = newSl;
    } else {
      if (price < state.peakPrice) state.peakPrice = price;
      const trailing = state.peakPrice * (1 + trailFactor);
      const newSl = state.currentSl === null ? trailing : Math.min(state.currentSl, trailing);
      if (state.currentSl === null || newSl < state.currentSl) {
        events.push(`Trailing SL updated: ${fmt(state.currentSl)} → ${fmt(newSl)} (trough=${fmt(state.peakPrice)})`);
      }
      state.currentSl = newSl;
    }
  }

  // --- 2. Trigger checks --------------------------------------------------
  let trigger: PositionState["triggeredReason"] = null;
  const tp = state.config.tp;
  const sl = state.currentSl;

  if (side === "long") {
    if (tp !== null && price >= tp) trigger = "TAKE PROFIT";
    else if (sl !== null && price <= sl) trigger = "STOP LOSS";
  } else {
    if (tp !== null && price <= tp) trigger = "TAKE PROFIT";
    else if (sl !== null && price >= sl) trigger = "STOP LOSS";
  }

  // --- 3. Commit tick ----------------------------------------------------
  state.currentPrice = price;
  state.pnl = computePnl(state.config, price);

  if (trigger !== null) {
    state.status = "triggered";
    state.triggeredReason = trigger;
    state.triggeredAt = now;
    const dir = side === "long" ? "SELL" : "BUY";
    events.push(
      `${trigger} @ ${fmt(price)} → would ${dir} ${state.config.quantity} ${state.config.market} (PnL=${state.pnl >= 0 ? "+" : ""}${fmt(state.pnl)})`,
    );
  }

  return { state, events };
}

/** Unrealized PnL if we exited at `price` right now. */
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
