// Port of agent-algo-order to TypeScript. Mirrors
// crates/agent-algo-order/src/main.rs (fn dispatcher + per-algo runners).
//
// Executes a plan of steps sequentially. Each step has one of three algos:
//  - twap: emit N equal slices at fixed intervals.
//  - iceberg: emit visible chunks; wait for each to "clear" before the next.
//  - liquidity-seeking: walk simulated depth each cycle; size the child by
//    slippage budget.
//
// The demo simulates each step's progress in real time.

export type AlgoKind = "twap" | "iceberg" | "liquidity-seeking";
export type Side = "buy" | "sell";

export type TwapStep = Readonly<{
  algorithm: "twap";
  market: string;
  side: Side;
  total: number;
  slices: number;
  durationSecs: number;
  priceOffsetPct: number;
}>;

export type IcebergStep = Readonly<{
  algorithm: "iceberg";
  market: string;
  side: Side;
  total: number;
  visible: number;
  price: number;
  pollSecs: number;
}>;

export type LiquiditySeekingStep = Readonly<{
  algorithm: "liquidity-seeking";
  market: string;
  side: Side;
  total: number;
  maxChunk: number;
  maxSlippageBps: number;
  depth: number;
  pollSecs: number;
}>;

export type Step = TwapStep | IcebergStep | LiquiditySeekingStep;

export type AlgoConfig = Readonly<{
  steps: Step[];
  startingPrice: number;
  bookVolatility: number;                // 0..0.01 per tick
}>;

export type ChildOrder = Readonly<{
  seq: number;
  stepIndex: number;
  algo: AlgoKind;
  market: string;
  side: "BID" | "OFFER";
  price: number;
  qty: number;
  placedAt: number;
  filledAt: number | null;
  filled: number;
  status: "active" | "filled" | "cancelled";
}>;

export type StepProgress = {
  index: number;
  algo: AlgoKind;
  market: string;
  side: Side;
  total: number;
  filled: number;
  status: "pending" | "running" | "done";
  childrenPlaced: number;
  startedAt: number | null;
  finishedAt: number | null;
  // Algo-specific transient state
  nextSliceAt: number;          // twap
  slicesDone: number;           // twap
  activeChildId: number | null; // iceberg + liq-seek — the one child we're waiting on
};

export type AlgoState = {
  config: AlgoConfig;
  status: "running" | "idle" | "done";
  currentPrice: number;
  currentStepIndex: number;
  steps: StepProgress[];
  children: ChildOrder[];       // bounded ~120
  nextChildSeq: number;
  totalPlaced: number;
  totalFilled: number;
  totalNotional: number;
};

const MAX_CHILDREN = 120;

export function initState(config: AlgoConfig, now: number): AlgoState {
  const _ = now;  // eslint-disable-line @typescript-eslint/no-unused-vars
  const steps: StepProgress[] = config.steps.map((s, i) => ({
    index: i,
    algo: s.algorithm,
    market: s.market,
    side: s.side,
    total: s.total,
    filled: 0,
    status: i === 0 ? "running" : "pending",
    childrenPlaced: 0,
    startedAt: i === 0 ? now : null,
    finishedAt: null,
    nextSliceAt: i === 0 && s.algorithm === "twap" ? now : 0,
    slicesDone: 0,
    activeChildId: null,
  }));
  return {
    config,
    status: steps.length > 0 ? "running" : "done",
    currentPrice: config.startingPrice,
    currentStepIndex: 0,
    steps,
    children: [],
    nextChildSeq: 1,
    totalPlaced: 0,
    totalFilled: 0,
    totalNotional: 0,
  };
}

/** Advance one tick. `driverPrice` walks the market mid. */
export function step(state: AlgoState, driverPrice: number, now: number): { state: AlgoState; log: string[] } {
  if (state.status !== "running") return { state, log: [] };
  const log: string[] = [];
  state.currentPrice = driverPrice;

  // Fill active children — 20% of remaining each tick.
  for (const child of state.children) {
    if (child.status !== "active") continue;
    const stepDef = state.config.steps[child.stepIndex];
    // Very simple fill model — respects the child's price limit relative to mid.
    const canFill = (child.side === "BID" && state.currentPrice <= child.price * 1.0005)
                 || (child.side === "OFFER" && state.currentPrice >= child.price * 0.9995);
    if (!canFill) continue;
    const remaining = child.qty - child.filled;
    const fillNow = Math.min(remaining, Math.max(remaining * 0.25, 0.01));
    // Mutation in-place — child is a Readonly union at the surface but this is a demo simulation.
    (child as { filled: number }).filled = child.filled + fillNow;
    if (child.qty - child.filled <= 0.001) {
      (child as { status: ChildOrder["status"] }).status = "filled";
      (child as { filledAt: number | null }).filledAt = now;
      state.totalFilled += fillNow;
      state.totalNotional += child.filled * child.price;
      log.push(`FILL  #${child.seq} ${child.side} ${stepDef.market} qty=${child.qty.toFixed(3)} @${child.price.toFixed(6)}`);
    } else {
      state.totalFilled += fillNow;
    }
    // Propagate the fill to the parent step.
    state.steps[child.stepIndex].filled += fillNow;
  }

  // Advance current step.
  const idx = state.currentStepIndex;
  if (idx < state.steps.length) {
    const sp = state.steps[idx];
    const def = state.config.steps[idx];
    const done = advanceStep(state, sp, def, now, log);
    if (done) {
      sp.status = "done";
      sp.finishedAt = now;
      log.push(`STEP  ${idx + 1}/${state.steps.length} ${sp.algo} complete — filled ${sp.filled.toFixed(3)}/${sp.total}`);
      state.currentStepIndex += 1;
      if (state.currentStepIndex < state.steps.length) {
        const next = state.steps[state.currentStepIndex];
        next.status = "running";
        next.startedAt = now;
        if (state.config.steps[state.currentStepIndex].algorithm === "twap") next.nextSliceAt = now;
      } else {
        state.status = "done";
        log.push(`PLAN  complete — ${state.totalPlaced} children placed, ${state.totalFilled.toFixed(3)} filled`);
      }
    }
  }

  // Bound children list.
  while (state.children.length > MAX_CHILDREN) state.children.shift();

  return { state, log };
}

function advanceStep(state: AlgoState, sp: StepProgress, def: Step, now: number, log: string[]): boolean {
  if (sp.filled >= sp.total - 0.001) return true;
  switch (def.algorithm) {
    case "twap": {
      if (now < sp.nextSliceAt) return false;
      if (sp.slicesDone >= def.slices) return sp.filled >= sp.total - 0.001;
      const sliceQty = def.total / def.slices;
      const price = state.currentPrice * (1 + def.priceOffsetPct / 100);
      placeChild(state, sp, def, price, sliceQty, now, log);
      sp.slicesDone += 1;
      const intervalMs = (def.durationSecs / def.slices) * 1000;
      sp.nextSliceAt = now + intervalMs;
      return false;
    }
    case "iceberg": {
      // Wait for the active child; when it clears, place the next visible chunk.
      if (sp.activeChildId !== null) {
        const active = state.children.find((c) => c.seq === sp.activeChildId);
        if (active && active.status === "active") return false;
        sp.activeChildId = null;
      }
      const remaining = def.total - sp.filled;
      const qty = Math.min(def.visible, remaining);
      if (qty <= 0.001) return true;
      const child = placeChild(state, sp, def, def.price, qty, now, log);
      sp.activeChildId = child.seq;
      return false;
    }
    case "liquidity-seeking": {
      // Wait for active child. Otherwise, size next chunk by simulated depth.
      if (sp.activeChildId !== null) {
        const active = state.children.find((c) => c.seq === sp.activeChildId);
        if (active && active.status === "active") return false;
        sp.activeChildId = null;
      }
      // Simulate depth: available = maxChunk × slippage-scaled fraction.
      const slippageFrac = Math.min(1, def.maxSlippageBps / 30);
      const available = def.maxChunk * (0.4 + 0.6 * Math.random()) * slippageFrac;
      const remaining = def.total - sp.filled;
      const qty = Math.min(available, def.maxChunk, remaining);
      if (qty <= 0.001) return sp.filled >= sp.total - 0.001;
      const price = state.currentPrice * (1 + (def.side === "buy" ? 0.0002 : -0.0002));
      const child = placeChild(state, sp, def, price, qty, now, log);
      sp.activeChildId = child.seq;
      return false;
    }
  }
}

function placeChild(state: AlgoState, sp: StepProgress, def: Step, price: number, qty: number, now: number, log: string[]): ChildOrder {
  const side: "BID" | "OFFER" = def.side === "buy" ? "BID" : "OFFER";
  const child: ChildOrder = {
    seq: state.nextChildSeq++,
    stepIndex: sp.index,
    algo: def.algorithm,
    market: def.market,
    side,
    price,
    qty,
    placedAt: now,
    filledAt: null,
    filled: 0,
    status: "active",
  };
  state.children.push(child);
  state.totalPlaced += 1;
  sp.childrenPlaced += 1;
  log.push(`PLACE #${child.seq} step=${sp.index + 1} ${def.algorithm} ${side} ${def.market} qty=${qty.toFixed(3)} @${price.toFixed(6)}`);
  return child;
}
