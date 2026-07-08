// Port of agent-block-execution-twap-iceberg placement logic to TypeScript. Mirrors
// crates/agent-block-execution-twap-iceberg/src/main.rs (fn block_loop).
//
// Rule (TWAP × Iceberg hybrid):
//   * Parent quantity `total` is split into `timeSlices` equal slices.
//   * Each slice runs for `durationSecs / timeSlices` seconds.
//   * Within a slice at most `visible` qty is live at price `config.price`.
//   * When a chunk fills, the next chunk (min(visible, sliceRemaining))
//     is posted until the slice's quantity is exhausted.
//   * If the slice's window expires with quantity left, the unfilled
//     remainder rolls into the next slice (carry-over).

export type Side = "buy" | "sell";
export type OrderType = "BID" | "OFFER";

export type BlockExecutionConfig = Readonly<{
  market: string;
  side: Side;
  total: number;
  price: number;         // limit price used for every chunk
  timeSlices: number;    // >= 1
  durationSecs: number;  // > 0, total wall-clock budget across all slices
  visible: number;       // max qty live per moment (>= 0, <= total)
  startingPrice: number; // seed for the price simulator
}>;

export type BlockOrder = {
  seq: number;
  t: number;              // epoch ms of placement
  side: OrderType;
  price: number;          // == config.price
  qty: number;            // visible chunk qty
  status: "open" | "filled" | "cancelled";
  slice: number;          // 1-based slice index
  filledAt?: number;
  filledPrice?: number;
};

export type BlockExecutionState = {
  config: BlockExecutionConfig;
  status: "monitoring" | "completed" | "idle";
  currentPrice: number;
  sliceQty: number;          // total / timeSlices
  sliceIntervalMs: number;   // (durationSecs * 1000) / timeSlices
  currentSlice: number;      // 1..timeSlices
  sliceStartAt: number;      // epoch ms when current slice started
  sliceRemaining: number;    // qty left in current slice (incl. carry-over)
  totalFilled: number;
  chunksPlaced: number;
  chunksFilled: number;
  currentChunkOrder: BlockOrder | null;
  orders: BlockOrder[];
  startedAt?: number;
  completedAt?: number;
};

const MAX_ORDERS = 200;

export function initState(config: BlockExecutionConfig, startPrice: number, now: number): BlockExecutionState {
  const sliceQty = config.total / config.timeSlices;
  const sliceIntervalMs = (config.durationSecs * 1000) / config.timeSlices;
  return {
    config,
    status: "monitoring",
    currentPrice: startPrice,
    sliceQty,
    sliceIntervalMs,
    currentSlice: 1,
    sliceStartAt: now,
    sliceRemaining: sliceQty,
    totalFilled: 0,
    chunksPlaced: 0,
    chunksFilled: 0,
    currentChunkOrder: null,
    orders: [],
    startedAt: now,
  };
}

/** Applies a tick. Returns state + emitted events. */
export function step(
  state: BlockExecutionState,
  price: number,
  now: number,
): { state: BlockExecutionState; events: string[] } {
  if (state.status !== "monitoring" || price <= 0) return { state, events: [] };
  const events: string[] = [];
  state.currentPrice = price;

  const { config } = state;
  const sideLabel: OrderType = config.side === "buy" ? "BID" : "OFFER";

  // 1) Detect slice roll based on wall-clock. Any unfilled qty of the
  //    current slice carries over into the next slice.
  if (state.startedAt !== undefined) {
    const elapsed = now - state.startedAt;
    const targetSlice = Math.min(config.timeSlices, Math.max(1, Math.floor(elapsed / state.sliceIntervalMs) + 1));
    while (state.currentSlice < targetSlice) {
      const carry = state.sliceRemaining;
      state.currentSlice += 1;
      state.sliceStartAt = state.startedAt + (state.currentSlice - 1) * state.sliceIntervalMs;
      state.sliceRemaining = state.sliceQty + carry;
      if (carry > 1e-12) {
        events.push(
          `SLICE ${state.currentSlice - 1} window expired — rolling ${fmt(carry)} unfilled into slice ${state.currentSlice}`,
        );
      }
      events.push(
        `SLICE ${state.currentSlice}/${config.timeSlices} start: target=${fmt(state.sliceRemaining)} window=${(state.sliceIntervalMs / 1000).toFixed(1)}s`,
      );
    }
  }

  // 2) Fill detection on the currently open chunk. BID fills when
  //    mid <= chunk.price, OFFER fills when mid >= chunk.price.
  const open = state.currentChunkOrder;
  if (open && open.status === "open") {
    const filled = open.side === "BID" ? price <= open.price : price >= open.price;
    if (filled) {
      open.status = "filled";
      open.filledAt = now;
      open.filledPrice = price;
      state.totalFilled += open.qty;
      state.chunksFilled += 1;
      state.sliceRemaining = Math.max(0, state.sliceRemaining - open.qty);
      state.currentChunkOrder = null;
      events.push(
        `FILL chunk #${open.seq} ${open.side} ${fmt(open.qty)} @ ${fmt(open.price)} (mid=${fmt(price)}) — slice=${open.slice} parent=${fmt(state.totalFilled)}/${fmt(config.total)}`,
      );
    }
  }

  // 3) Completion checks.
  const remainingParent = Math.max(0, config.total - state.totalFilled);
  if (remainingParent <= 1e-12) {
    state.status = "completed";
    state.completedAt = now;
    events.push(`COMPLETE: parent filled ${fmt(state.totalFilled)}/${fmt(config.total)}`);
    return { state, events };
  }
  if (state.currentSlice >= config.timeSlices) {
    // We're in the final slice; if its window has fully expired and no chunk is live, we're done.
    const finalSliceEnd = (state.startedAt ?? now) + config.timeSlices * state.sliceIntervalMs;
    if (now >= finalSliceEnd && !state.currentChunkOrder && state.sliceRemaining <= 1e-12) {
      state.status = "completed";
      state.completedAt = now;
      events.push(
        `COMPLETE: schedule ended — parent filled ${fmt(state.totalFilled)}/${fmt(config.total)}`,
      );
      return { state, events };
    }
  }

  // 4) Post next chunk when nothing is live and slice has remaining qty.
  if (!state.currentChunkOrder && state.sliceRemaining > 1e-12) {
    const chunkQty = Math.min(config.visible, state.sliceRemaining, remainingParent);
    if (chunkQty > 1e-12) {
      state.chunksPlaced += 1;
      const seq = state.chunksPlaced;
      const order: BlockOrder = {
        seq,
        t: now,
        side: sideLabel,
        price: config.price,
        qty: round8(chunkQty),
        status: "open",
        slice: state.currentSlice,
      };
      state.orders.push(order);
      state.currentChunkOrder = order;
      if (state.orders.length > MAX_ORDERS) state.orders.shift();
      events.push(
        `CHUNK #${seq} slice=${state.currentSlice} ${sideLabel} ${fmt(order.qty)} @ ${fmt(order.price)} (mid=${fmt(price)}, slice_remaining=${fmt(state.sliceRemaining)})`,
      );
    }
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
