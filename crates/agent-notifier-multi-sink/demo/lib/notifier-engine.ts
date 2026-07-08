// Port of agent-notifier-multi-sink logic to TypeScript. Mirrors
// crates/agent-notifier-multi-sink/src/main.rs — subscribes to orders/settlements/prices
// and dispatches each event to configured sinks: stdout / file / webhook.
//
// The demo simulates the upstream event streams and each sink's success/failure
// so operators can see delivery ratios in real time.

export type SinkKind = "stdout" | "file" | "webhook";

export type Sink = {
  kind: SinkKind;
  label: string;         // e.g. "stdout" or "file:events.jsonl" or "webhook:https://…"
  target?: string;       // path for file, url for webhook
  deliveredCount: number;
  failedCount: number;
};

export type NotifierConfig = Readonly<{
  orders: boolean;
  settlements: boolean;
  prices: boolean;
  sinks: ReadonlyArray<Readonly<{ kind: SinkKind; label: string; target?: string }>>;
  market?: string;              // optional filter (empty => no filter)
  startingPrice: number;
  webhookFailureRate: number;   // 0..1 — probability a webhook POST fails
}>;

export type EventKind =
  | "order.created"
  | "order.filled"
  | "order.cancelled"
  | "settlement.proposal"
  | "settlement.settled"
  | "settlement.failed"
  | "price.update";

export type EventItem = Readonly<{
  seq: number;
  t: number;
  kind: EventKind;
  payload: Record<string, unknown>;
}>;

export type NotifierState = {
  config: NotifierConfig;
  status: "monitoring" | "idle";
  currentPrice: number;
  totalEvents: number;
  totalDelivered: number;
  totalFailed: number;
  lastEventKind: EventKind | null;
  eventsPerMinute: number;
  events: EventItem[];         // bounded ~60
  sinks: Sink[];               // per-sink counters
};

const MAX_EVENTS = 60;

// Sample events templates by category. Chosen so both order + settlement +
// price flows are exercised.
const ORDER_KINDS: EventKind[] = ["order.created", "order.filled", "order.cancelled"];
const SETTLEMENT_KINDS: EventKind[] = ["settlement.proposal", "settlement.settled", "settlement.failed"];

export function initState(config: NotifierConfig): NotifierState {
  const sinks: Sink[] = config.sinks.map((s) => ({ ...s, deliveredCount: 0, failedCount: 0 }));
  return {
    config,
    status: "monitoring",
    currentPrice: config.startingPrice,
    totalEvents: 0,
    totalDelivered: 0,
    totalFailed: 0,
    lastEventKind: null,
    eventsPerMinute: 0,
    events: [],
    sinks,
  };
}

/** Applies one tick. Simulates upstream events + per-sink delivery. */
export function step(state: NotifierState, price: number, now: number): { state: NotifierState; log: string[] } {
  if (state.status !== "monitoring" || price <= 0) return { state, log: [] };
  const log: string[] = [];
  state.currentPrice = price;

  // Decide which categories are enabled + build candidate events for this tick.
  const candidates: Array<{ kind: EventKind; payload: Record<string, unknown> }> = [];

  // Price ticks — one per tick when enabled.
  if (state.config.prices) {
    candidates.push({
      kind: "price.update",
      payload: {
        market_id: state.config.market ?? "CC-USDC",
        price: round8(price),
        bid: round8(price * 0.999),
        ask: round8(price * 1.001),
        source: "silvana.oracle",
      },
    });
  }
  // Orders — random burst per tick when enabled.
  if (state.config.orders && Math.random() < 0.35) {
    const kind = ORDER_KINDS[Math.floor(Math.random() * ORDER_KINDS.length)];
    candidates.push({
      kind,
      payload: {
        order_id: `ord_${(state.totalEvents + candidates.length + 1).toString(16)}`,
        market_id: state.config.market ?? "CC-USDC",
        side: Math.random() < 0.5 ? "BID" : "OFFER",
        price: round8(price * (1 + (Math.random() - 0.5) * 0.01)),
        quantity: Math.round(Math.random() * 500) / 100 + 0.01,
      },
    });
  }
  // Settlements — rarer.
  if (state.config.settlements && Math.random() < 0.2) {
    const kind = SETTLEMENT_KINDS[Math.floor(Math.random() * SETTLEMENT_KINDS.length)];
    candidates.push({
      kind,
      payload: {
        proposal_id: `prop_${(state.totalEvents + candidates.length + 1).toString(16)}`,
        market_id: state.config.market ?? "CC-USDC",
        buyer: "party::alice",
        seller: "party::bob",
        base_quantity: Math.round(Math.random() * 300) / 100 + 0.5,
        settlement_price: round8(price),
        status: kind === "settlement.failed" ? "FAILED" : kind === "settlement.settled" ? "SETTLED" : "PENDING",
      },
    });
  }

  // Dispatch every candidate to every sink.
  for (const c of candidates) {
    const seq = state.totalEvents + 1;
    const item: EventItem = { seq, t: now, kind: c.kind, payload: c.payload };
    state.events.push(item);
    if (state.events.length > MAX_EVENTS) state.events.shift();
    state.totalEvents = seq;
    state.lastEventKind = c.kind;

    for (const sink of state.sinks) {
      const willFail = sink.kind === "webhook" && Math.random() < state.config.webhookFailureRate;
      if (willFail) {
        sink.failedCount += 1;
        state.totalFailed += 1;
        log.push(`SINK-FAIL [${sink.label}] ${c.kind} — HTTP 5xx`);
      } else {
        sink.deliveredCount += 1;
        state.totalDelivered += 1;
        log.push(`NOTIFIED [${sink.label}] ${c.kind}`);
      }
    }
  }

  // Simple rolling events/min estimate — 60-tick average scaled to a minute.
  const windowMs = 60_000;
  const cutoff = now - windowMs;
  const inWindow = state.events.filter((e) => e.t >= cutoff).length;
  state.eventsPerMinute = inWindow;

  return { state, log };
}

function round8(n: number): number {
  return Math.round(n * 1e8) / 1e8;
}
