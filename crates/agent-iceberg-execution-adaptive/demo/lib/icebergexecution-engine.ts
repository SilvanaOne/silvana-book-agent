// Port of agent-iceberg-execution-adaptive to TypeScript. Mirrors
// crates/agent-iceberg-execution-adaptive/src/main.rs (fn iceberg_loop).
//
// Rule:
//   Split `total` parent order into successive chunks. At most one chunk is
//   on-book at a time. Each chunk is placed at `price`. Fill detection (fake
//   tape):
//     BID   filled when mid <= chunk.price
//     OFFER filled when mid >= chunk.price
//
//   The next chunk's visible quantity ADAPTS to the previous chunk's fill
//   velocity:
//     elapsed < fastSecs → nextVisible = min(current × 2, maxVisible)
//     elapsed > slowSecs → nextVisible = max(current / 2, minVisible)
//     otherwise          → unchanged
//
//   Stop when totalFilled >= total, or when runtime > maxRuntimeSecs.

export type Side = "buy" | "sell";
export type OrderType = "BID" | "OFFER";
export type ChunkStatus = "open" | "filled" | "cancelled";

export type IcebergExecutionConfig = Readonly<{
  market: string;
  side: Side;
  total: number;
  initialVisible: number;
  minVisible: number;
  maxVisible: number;
  fastSecs: number;
  slowSecs: number;
  price: number;
  maxRuntimeSecs: number;
  startingPrice: number;
}>;

export type IcebergOrder = {
  seq: number;
  t: number;              // epoch ms of placement
  price: number;
  qty: number;
  side: OrderType;
  status: ChunkStatus;
  filledAt?: number;
  elapsedMs?: number;
};

export type IcebergExecutionState = {
  config: IcebergExecutionConfig;
  status: "monitoring" | "completed" | "idle";
  currentPrice: number;
  totalFilled: number;
  currentVisible: number;      // visible size that is (or will be) applied
  nextVisible: number;         // planned size for the NEXT chunk
  currentChunkOrder: IcebergOrder | null;
  lastChunkStartAt: number | null;
  lastChunkEndAt: number | null;
  lastChunkElapsedSecs: number | null;
  lastResizeReason: "grow" | "shrink" | "hold" | null;
  chunksPlaced: number;
  chunksFilled: number;
  orders: IcebergOrder[];   // bounded
  startedAt: number;
  completedAt?: number;
};

const MAX_ORDERS = 80;

export function initState(config: IcebergExecutionConfig, startPrice: number, now: number): IcebergExecutionState {
  return {
    config,
    status: "monitoring",
    currentPrice: startPrice,
    totalFilled: 0,
    currentVisible: clamp(config.initialVisible, config.minVisible, config.maxVisible),
    nextVisible: clamp(config.initialVisible, config.minVisible, config.maxVisible),
    currentChunkOrder: null,
    lastChunkStartAt: null,
    lastChunkEndAt: null,
    lastChunkElapsedSecs: null,
    lastResizeReason: null,
    chunksPlaced: 0,
    chunksFilled: 0,
    orders: [],
    startedAt: now,
  };
}

/** Applies a tick. Returns state + emitted events. */
export function step(state: IcebergExecutionState, price: number, now: number): { state: IcebergExecutionState; events: string[] } {
  if (state.status !== "monitoring" || price <= 0) return { state, events: [] };
  const events: string[] = [];
  state.currentPrice = price;

  // Max runtime guard.
  if ((now - state.startedAt) / 1000 > state.config.maxRuntimeSecs) {
    state.status = "completed";
    state.completedAt = now;
    events.push(
      `MAX RUNTIME REACHED — filled ${state.totalFilled.toFixed(6)} / ${state.config.total} in ${state.chunksFilled} chunk(s)`,
    );
    return { state, events };
  }

  // 1. Fill detection for the live chunk.
  const cur = state.currentChunkOrder;
  if (cur && cur.status === "open") {
    const filled = cur.side === "BID" ? price <= cur.price : price >= cur.price;
    if (filled) {
      cur.status = "filled";
      cur.filledAt = now;
      cur.elapsedMs = state.lastChunkStartAt !== null ? now - state.lastChunkStartAt : 0;
      state.chunksFilled += 1;
      state.totalFilled += cur.qty;
      state.lastChunkEndAt = now;
      const elapsedSecs = Math.max(0, Math.floor((cur.elapsedMs ?? 0) / 1000));
      state.lastChunkElapsedSecs = elapsedSecs;

      // Adaptive resize for the NEXT chunk.
      const prev = state.currentVisible;
      let next = prev;
      let reason: "grow" | "shrink" | "hold" = "hold";
      if (elapsedSecs < state.config.fastSecs) {
        next = Math.min(prev * 2, state.config.maxVisible);
        reason = next > prev ? "grow" : "hold";
      } else if (elapsedSecs > state.config.slowSecs) {
        next = Math.max(prev / 2, state.config.minVisible);
        reason = next < prev ? "shrink" : "hold";
      }
      next = round8(next);
      state.nextVisible = next;
      state.lastResizeReason = reason;
      state.currentChunkOrder = null;

      events.push(
        `FILL CHUNK #${cur.seq}: ${cur.side} ${fmt(cur.qty)} @ ${fmt(cur.price)} in ${elapsedSecs}s — total ${fmt(state.totalFilled)}/${state.config.total}`,
      );
      if (reason !== "hold" && next !== prev) {
        events.push(
          `ADAPT ${reason.toUpperCase()}: elapsed ${elapsedSecs}s → next visible ${fmt(prev)} → ${fmt(next)}`,
        );
      }
    }
  }

  // 2. Completion check.
  if (state.totalFilled >= state.config.total) {
    state.status = "completed";
    state.completedAt = now;
    events.push(
      `ICEBERG COMPLETE — ${state.chunksFilled} chunks, total filled ${fmt(state.totalFilled)}`,
    );
    return { state, events };
  }

  // 3. Place next chunk if none open.
  if (state.currentChunkOrder === null) {
    const remaining = state.config.total - state.totalFilled;
    // The new chunk uses `nextVisible`; adopt it as the currentVisible now.
    const targetVisible = clamp(state.nextVisible, state.config.minVisible, state.config.maxVisible);
    state.currentVisible = targetVisible;
    const qty = Math.min(targetVisible, remaining);
    if (qty > 0) {
      const seq = state.chunksPlaced + 1;
      const side: OrderType = state.config.side === "buy" ? "BID" : "OFFER";
      const order: IcebergOrder = {
        seq,
        t: now,
        price: round8(state.config.price),
        qty: round8(qty),
        side,
        status: "open",
      };
      state.orders.push(order);
      if (state.orders.length > MAX_ORDERS) state.orders.shift();
      state.currentChunkOrder = order;
      state.chunksPlaced = seq;
      state.lastChunkStartAt = now;
      events.push(
        `CHUNK #${seq}: ${side} ${fmt(order.qty)} @ ${fmt(order.price)} (visible ${fmt(targetVisible)}, remaining ${fmt(remaining)})`,
      );
    }
  }

  return { state, events };
}

function clamp(n: number, lo: number, hi: number): number {
  if (n < lo) return lo;
  if (n > hi) return hi;
  return n;
}

function round8(n: number): number {
  return Math.round(n * 1e8) / 1e8;
}

function fmt(n: number): string {
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? 4 : 6;
  return n.toFixed(digits);
}
