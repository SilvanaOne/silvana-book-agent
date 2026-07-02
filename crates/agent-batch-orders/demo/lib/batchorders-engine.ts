// Port of agent-batch-orders logic to TypeScript. Mirrors the CLI in
// crates/agent-batch-orders/src/main.rs: submit_batch / cancel_batch /
// cancel_all. This demo drains a JSONL batch of orders through a
// simulated venue, letting the operator watch pending → submitted →
// filled (or failed / cancelled) live.

export type Side = "buy" | "sell";
export type OrderType = "BID" | "OFFER";
export type BatchOrderStatus =
  | "pending"
  | "submitted"
  | "failed"
  | "cancelled"
  | "filled";

export type OrderSpec = Readonly<{
  market: string;
  side: Side;
  quantity: number;
  price: number;
  ref?: string;
}>;

export type BatchOrder = {
  seq: number;
  t: number;
  market: string;
  side: OrderType;
  quantity: number;
  price: number;
  ref?: string;
  status: BatchOrderStatus;
  submittedAt?: number;
  filledAt?: number;
};

export type BatchOrdersConfig = Readonly<{
  orders: readonly OrderSpec[];
  submitRatePerTick: number;   // pending → submitted per tick
  abortOnError: boolean;
  failureRatePerTick: number;  // probability that a submission fails
  startingPrice: number;
}>;

export type BatchStats = {
  pending: number;
  submitted: number;
  failed: number;
  cancelled: number;
  filled: number;
};

export type BatchOrdersState = {
  config: BatchOrdersConfig;
  status: "idle" | "submitting" | "monitoring" | "cancelled" | "completed";
  currentPrice: number;
  batch: BatchOrder[];
  stats: BatchStats;
  submittedCount: number;   // cumulative submissions attempted (for throughput)
  cancelSignal: boolean;
  startedAt: number;
  lastTickAt: number;
};

export function initState(
  config: BatchOrdersConfig,
  startPrice: number,
  now: number,
): BatchOrdersState {
  const batch: BatchOrder[] = config.orders.map((o, i) => ({
    seq: i + 1,
    t: now,
    market: o.market,
    side: o.side === "buy" ? "BID" : "OFFER",
    quantity: o.quantity,
    price: o.price,
    ref: o.ref,
    status: "pending",
  }));
  return {
    config,
    status: batch.length === 0 ? "completed" : "submitting",
    currentPrice: startPrice,
    batch,
    stats: recomputeStats(batch),
    submittedCount: 0,
    cancelSignal: false,
    startedAt: now,
    lastTickAt: now,
  };
}

/** Applies a tick. Returns state + emitted events. */
export function step(
  state: BatchOrdersState,
  price: number,
  now: number,
): { state: BatchOrdersState; events: string[] } {
  const events: string[] = [];
  if (
    state.status === "cancelled" ||
    state.status === "completed" ||
    state.status === "idle"
  ) {
    return { state, events };
  }
  if (price > 0) state.currentPrice = price;
  state.lastTickAt = now;

  // 1) Cancel signal — mark every non-terminal order as cancelled
  if (state.cancelSignal) {
    for (const o of state.batch) {
      if (o.status === "pending" || o.status === "submitted") {
        o.status = "cancelled";
        events.push(
          `CANCEL #${o.seq} ${o.side} ${o.market} ${fmt(o.quantity)} @ ${fmt(o.price)}`,
        );
      }
    }
    state.cancelSignal = false;
    state.status = "cancelled";
    state.stats = recomputeStats(state.batch);
    return { state, events };
  }

  // 2) Submit up to submitRatePerTick pending → submitted (or failed)
  let submittedThisTick = 0;
  let aborted = false;
  for (const o of state.batch) {
    if (submittedThisTick >= state.config.submitRatePerTick) break;
    if (o.status !== "pending") continue;
    state.submittedCount += 1;
    submittedThisTick += 1;
    const failed = Math.random() < state.config.failureRatePerTick;
    if (failed) {
      o.status = "failed";
      events.push(
        `FAIL #${o.seq} ${o.side} ${o.market} ${fmt(o.quantity)} @ ${fmt(o.price)} — submit error`,
      );
      if (state.config.abortOnError) {
        aborted = true;
        // Cancel remaining pending
        for (const rest of state.batch) {
          if (rest.status === "pending") {
            rest.status = "cancelled";
            events.push(
              `ABORT #${rest.seq} ${rest.side} ${rest.market} — abort_on_error`,
            );
          }
        }
        break;
      }
      continue;
    }
    o.status = "submitted";
    o.submittedAt = now;
    events.push(
      `OK #${o.seq} ${o.side} ${o.market} ${fmt(o.quantity)} @ ${fmt(o.price)}${o.ref ? ` (ref=${o.ref})` : ""}`,
    );
  }

  // 3) Fill sweep: BID fills when price ≤ o.price; OFFER when price ≥ o.price
  for (const o of state.batch) {
    if (o.status !== "submitted") continue;
    const filled = o.side === "BID" ? price <= o.price : price >= o.price;
    if (!filled) continue;
    o.status = "filled";
    o.filledAt = now;
    events.push(
      `FILL #${o.seq} ${o.side} ${o.market} ${fmt(o.quantity)} @ ${fmt(o.price)} (mid=${fmt(price)})`,
    );
  }

  // 4) Recompute stats / status
  state.stats = recomputeStats(state.batch);
  if (aborted) {
    // If abort-on-error, no more pending → move to monitoring/completed
    if (state.stats.pending === 0 && state.stats.submitted === 0) {
      state.status = "completed";
    } else {
      state.status = "monitoring";
    }
  } else if (state.stats.pending === 0) {
    if (state.stats.submitted === 0) state.status = "completed";
    else state.status = "monitoring";
  }
  return { state, events };
}

function recomputeStats(batch: readonly BatchOrder[]): BatchStats {
  const stats: BatchStats = { pending: 0, submitted: 0, failed: 0, cancelled: 0, filled: 0 };
  for (const o of batch) stats[o.status] += 1;
  return stats;
}

/** Parse a JSONL blob into OrderSpec[]. Throws on the first bad line. */
export function parseOrdersJsonl(text: string): OrderSpec[] {
  const out: OrderSpec[] = [];
  const lines = text.split(/\r?\n/);
  for (let i = 0; i < lines.length; i++) {
    const raw = lines[i];
    const trimmed = raw.trim();
    if (trimmed === "" || trimmed.startsWith("#")) continue;
    let obj: Record<string, unknown>;
    try {
      obj = JSON.parse(trimmed) as Record<string, unknown>;
    } catch (e) {
      throw new Error(`line ${i + 1}: malformed JSON — ${(e as Error).message}`);
    }
    const market = obj.market;
    const side = obj.side;
    const q = obj.quantity;
    const p = obj.price;
    if (typeof market !== "string" || market === "") {
      throw new Error(`line ${i + 1}: missing "market"`);
    }
    if (side !== "buy" && side !== "sell") {
      throw new Error(`line ${i + 1}: "side" must be "buy" or "sell"`);
    }
    const qty = typeof q === "number" ? q : Number(q);
    const price = typeof p === "number" ? p : Number(p);
    if (!Number.isFinite(qty) || qty <= 0) {
      throw new Error(`line ${i + 1}: "quantity" must be a positive number`);
    }
    if (!Number.isFinite(price) || price <= 0) {
      throw new Error(`line ${i + 1}: "price" must be a positive number`);
    }
    out.push({
      market,
      side,
      quantity: qty,
      price,
      ref: typeof obj.ref === "string" ? obj.ref : undefined,
    });
  }
  return out;
}

export function meanPrice(batch: readonly BatchOrder[], side: OrderType): number | null {
  const arr = batch.filter((o) => o.side === side);
  if (arr.length === 0) return null;
  let s = 0;
  for (const o of arr) s += o.price;
  return s / arr.length;
}

function fmt(n: number): string {
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? 4 : 6;
  return n.toFixed(digits);
}
