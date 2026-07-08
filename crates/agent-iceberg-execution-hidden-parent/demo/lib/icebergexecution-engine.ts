// Port of agent-iceberg-execution-hidden-parent to TypeScript. Mirrors
// crates/agent-iceberg-execution-hidden-parent/src/main.rs (fn iceberg_loop).
//
// Rule:
//   Split `total` parent order into successive `visible` chunks.
//   At most one chunk is on-book at a time. When chunk N is filled
//   (or cancelled), chunk N+1 is placed. Fill detection (fake tape):
//     BID   filled when mid <= chunk.price
//     OFFER filled when mid >= chunk.price
//   Stop when totalFilled >= total, or when runtime > maxRuntimeSecs.

export type Side = "buy" | "sell";
export type OrderType = "BID" | "OFFER";
export type ChunkStatus = "open" | "filled" | "cancelled";

export type IcebergExecutionConfig = Readonly<{
  market: string;
  side: Side;
  total: number;
  visible: number;
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
};

export type IcebergExecutionState = {
  config: IcebergExecutionConfig;
  status: "monitoring" | "completed" | "idle";
  currentPrice: number;
  totalFilled: number;
  currentChunkQty: number;
  currentChunkOrder: IcebergOrder | null;
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
    currentChunkQty: 0,
    currentChunkOrder: null,
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
      state.chunksFilled += 1;
      state.totalFilled += cur.qty;
      events.push(
        `FILL CHUNK #${cur.seq}: ${cur.side} ${cur.qty} @ ${fmt(cur.price)} (mid=${fmt(price)}) — total filled ${fmt(state.totalFilled)}/${state.config.total}`,
      );
      state.currentChunkOrder = null;
      state.currentChunkQty = 0;
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
    const qty = Math.min(state.config.visible, remaining);
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
      state.currentChunkQty = order.qty;
      state.chunksPlaced = seq;
      events.push(
        `CHUNK #${seq}: ${side} ${fmt(order.qty)} @ ${fmt(order.price)} (remaining ${fmt(remaining)})`,
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
