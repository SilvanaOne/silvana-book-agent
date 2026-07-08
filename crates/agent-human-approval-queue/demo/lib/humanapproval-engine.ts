// Port of agent-human-approval-queue flow to TypeScript. Mirrors
// crates/agent-human-approval-queue/src/main.rs (enqueue / approve / reject / purge).
//
// Model:
//   - Upstream agents enqueue orders into a JSONL queue. Each entry starts
//     as `pending`. A human operator inspects, then approves or rejects.
//   - `approve` would sign and submit via OrderbookService.SubmitOrder.
//   - `reject` never submits.
//   - Optional auto-approval: entries whose notional is under a configured
//     threshold get auto-approved as they arrive (still logged).
//
// The demo simulates upstream enqueues (Poisson-ish arrival) and lets the
// operator approve/reject each pending row from the UI.

export type Side = "BID" | "OFFER";
export type OrderStatus = "pending" | "approved" | "rejected";

export type PendingOrder = {
  id: string;
  receivedAt: number;      // epoch ms enqueued
  market: string;
  side: Side;
  quantity: number;
  price: number;
  notional: number;        // quantity * price
  ref?: string;
  submittedBy: string;     // upstream agent that enqueued
  status: OrderStatus;
  decidedBy?: string;
  decidedAt?: number;
  decisionReason?: string;
};

export type HumanApprovalConfig = Readonly<{
  market: string;
  orderArrivalPerTick: number;   // expected count per tick (Poisson-ish)
  autoApprovalEnabled: boolean;
  autoApprovalThreshold: number; // notional < this → auto-approve when enabled
  reviewerName: string;          // display name of the operator
  startingPrice: number;
}>;

export type HumanApprovalStats = {
  pending: number;
  approved: number;
  rejected: number;
  autoApproved: number;
  submitted: number;
  totalReceived: number;
};

export type HumanApprovalState = {
  config: HumanApprovalConfig;
  status: "monitoring" | "idle";
  currentPrice: number;
  queue: PendingOrder[];      // pending queue (bounded ~30)
  history: PendingOrder[];    // decided (bounded ~40, most recent last)
  stats: HumanApprovalStats;
  seq: number;                // internal id counter
};

const MAX_QUEUE = 30;
const MAX_HISTORY = 40;
const UPSTREAM_AGENTS = ["dca-bot", "spot-grid", "hedging"] as const;

export function initState(config: HumanApprovalConfig, startPrice: number): HumanApprovalState {
  return {
    config,
    status: "monitoring",
    currentPrice: startPrice,
    queue: [],
    history: [],
    stats: { pending: 0, approved: 0, rejected: 0, autoApproved: 0, submitted: 0, totalReceived: 0 },
    seq: 0,
  };
}

/**
 * Poisson-ish sampler: returns the count of arrivals for this tick given
 * a target rate `lambda`. Uses Knuth's algorithm — cheap for small lambda.
 */
function poissonSample(lambda: number): number {
  if (lambda <= 0) return 0;
  // For very large lambdas the algorithm is slow; the UI caps at 10.
  const L = Math.exp(-lambda);
  let k = 0;
  let p = 1;
  while (true) {
    k += 1;
    p *= Math.random();
    if (p <= L) return k - 1;
  }
}

function pickAgent(): string {
  return UPSTREAM_AGENTS[Math.floor(Math.random() * UPSTREAM_AGENTS.length)];
}

function randomSide(): Side {
  return Math.random() < 0.5 ? "BID" : "OFFER";
}

function randomQty(): number {
  // Skewed to smaller quantities so notional often lands below the
  // auto-approval threshold.
  const r = Math.random();
  const q = r < 0.6 ? 0.5 + Math.random() * 4.5
          : r < 0.9 ? 5 + Math.random() * 45
                    : 50 + Math.random() * 200;
  return round4(q);
}

function priceNear(mid: number, side: Side): number {
  // BID sits slightly under, OFFER slightly over, ±0..0.6% jitter.
  const bias = side === "BID" ? -1 : 1;
  const jitter = Math.random() * 0.006;
  return round8(mid * (1 + bias * jitter));
}

/** Applies a tick: samples new arrivals + auto-approves under-threshold ones. */
export function step(state: HumanApprovalState, price: number, now: number): { state: HumanApprovalState; events: string[] } {
  if (state.status !== "monitoring" || price <= 0) return { state, events: [] };
  const events: string[] = [];
  state.currentPrice = price;

  const arrivals = poissonSample(state.config.orderArrivalPerTick);
  for (let i = 0; i < arrivals; i++) {
    state.seq += 1;
    const seq = state.seq;
    const id = `ha-${now}-${seq}`;
    const side = randomSide();
    const qty = randomQty();
    const px = priceNear(price, side);
    const notional = round4(qty * px);
    const submittedBy = pickAgent();
    const order: PendingOrder = {
      id,
      receivedAt: now,
      market: state.config.market,
      side,
      quantity: qty,
      price: px,
      notional,
      ref: `${submittedBy}-${seq}`,
      submittedBy,
      status: "pending",
    };
    state.queue.push(order);
    state.stats.totalReceived += 1;
    events.push(
      `ENQUEUE #${idShort(id)} from ${submittedBy}: ${side} ${qty}@${fmt(px)} notional=${fmt(notional)}`,
    );

    // Auto-approval path — only under threshold and only when enabled.
    if (state.config.autoApprovalEnabled && notional < state.config.autoApprovalThreshold) {
      // Immediate decision: move from queue → history as approved by "auto".
      const idx = state.queue.findIndex((o) => o.id === order.id);
      if (idx >= 0) {
        const [taken] = state.queue.splice(idx, 1);
        taken.status = "approved";
        taken.decidedBy = "auto";
        taken.decidedAt = now;
        taken.decisionReason = `notional ${fmt(notional)} < threshold ${fmt(state.config.autoApprovalThreshold)}`;
        state.history.push(taken);
        state.stats.approved += 1;
        state.stats.autoApproved += 1;
        state.stats.submitted += 1;
        events.push(`AUTO-APPROVED #${idShort(taken.id)} → SubmitOrder (${taken.decisionReason})`);
      }
    }
  }

  // Cap sizes.
  while (state.queue.length > MAX_QUEUE) state.queue.shift();
  while (state.history.length > MAX_HISTORY) state.history.shift();

  state.stats.pending = state.queue.length;
  return { state, events };
}

/** Manual decision — invoked by the API. */
export function decide(
  state: HumanApprovalState,
  id: string,
  decision: "approve" | "reject",
  by: string | undefined,
  reason: string | undefined,
  now: number,
): { ok: boolean; event?: string; error?: string } {
  const idx = state.queue.findIndex((o) => o.id === id);
  if (idx < 0) return { ok: false, error: `no pending entry with id ${id}` };
  const [taken] = state.queue.splice(idx, 1);
  taken.status = decision === "approve" ? "approved" : "rejected";
  taken.decidedBy = by && by.length > 0 ? by : state.config.reviewerName;
  taken.decidedAt = now;
  taken.decisionReason = reason;
  state.history.push(taken);
  if (state.history.length > MAX_HISTORY) state.history.shift();

  if (decision === "approve") {
    state.stats.approved += 1;
    state.stats.submitted += 1;
  } else {
    state.stats.rejected += 1;
  }
  state.stats.pending = state.queue.length;

  const label = decision === "approve" ? "APPROVED" : "REJECTED";
  const suffix = decision === "approve" ? " → SubmitOrder" : "";
  const reasonStr = reason ? ` reason="${reason}"` : "";
  return {
    ok: true,
    event: `${label} #${idShort(taken.id)} by ${taken.decidedBy}${reasonStr}${suffix}`,
  };
}

/** Wipe the pending queue. Decided entries stay in history. */
export function purge(state: HumanApprovalState, now: number): { removed: number; event: string } {
  const n = state.queue.length;
  state.queue = [];
  state.stats.pending = 0;
  return { removed: n, event: `PURGE cleared ${n} pending entries at ${new Date(now).toISOString()}` };
}

function round4(n: number): number {
  return Math.round(n * 1e4) / 1e4;
}
function round8(n: number): number {
  return Math.round(n * 1e8) / 1e8;
}
function fmt(n: number): string {
  const abs = Math.abs(n);
  const digits = abs > 100 ? 2 : abs > 1 ? 4 : 6;
  return n.toFixed(digits);
}
export function idShort(id: string): string {
  // Trim the timestamp middle for compact display.
  const parts = id.split("-");
  if (parts.length < 3) return id;
  return `${parts[0]}-${parts[parts.length - 1]}`;
}
