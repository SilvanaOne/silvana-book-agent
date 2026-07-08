// Port of agent-witnesses-shell-hooks logic to TypeScript. Mirrors
// crates/agent-witnesses-shell-hooks/src/main.rs.
//
// The Rust agent subscribes to SubscribeOrders + SubscribeSettlements and
// spawns a shell command per matched event-type. Metadata is exported via
// SILVANA_EVENT, SILVANA_EVENT_TS, SILVANA_PROPOSAL_ID, SILVANA_ORDER_ID,
// SILVANA_MARKET_ID, SILVANA_STATUS env vars.
//
// This demo simulates the event stream (Poisson arrival per tick) and mocks
// command execution (duration + exit code with configurable failure rate).

export type Handler = Readonly<{
  eventKind: string;
  command: string;
}>;

export type CommandInvocation = Readonly<{
  seq: number;
  t: number;
  eventKind: string;
  command: string;
  exitCode: number;
  stdout: string;
  durationMs: number;
  env: Readonly<Record<string, string>>;
}>;

export type WitnessesConfig = Readonly<{
  handlers: readonly Handler[];
  market: string;                 // optional filter — "" means all markets
  eventArrivalPerTick: number;    // Poisson prob 0..1
  commandDurationMs: number;      // avg simulated duration
  commandFailureRate: number;     // 0..1 chance exit != 0
  startingPrice: number;
}>;

export type WitnessesStats = {
  triggered: number;
  succeeded: number;
  failed: number;
  byEventKind: Record<string, number>;
};

export type WitnessesState = {
  config: WitnessesConfig;
  status: "monitoring" | "idle";
  currentPrice: number;
  invocations: CommandInvocation[];  // bounded
  stats: WitnessesStats;
};

const MAX_INVOCATIONS = 30;

const EVENT_POOL: readonly string[] = [
  "settlement.settled",
  "settlement.failed",
  "settlement.cancelled",
  "settlement.proposal_created",
  "order.filled",
  "order.cancelled",
];

// Short-form aliases accepted from the form UI ("settled" → "settlement.settled" etc)
const ALIAS_TO_KIND: Readonly<Record<string, string>> = {
  "settled": "settlement.settled",
  "failed": "settlement.failed",
  "cancelled": "settlement.cancelled",
  "canceled": "settlement.cancelled",
  "proposal": "settlement.proposal_created",
  "proposal_created": "settlement.proposal_created",
  "order-filled": "order.filled",
  "order_filled": "order.filled",
  "order-cancelled": "order.cancelled",
  "order_cancelled": "order.cancelled",
};

export function normalizeEventKind(raw: string): string {
  const k = raw.trim().toLowerCase();
  return ALIAS_TO_KIND[k] ?? k;
}

export function parseHandlers(text: string): Handler[] {
  const out: Handler[] = [];
  for (const rawLine of text.split(/\r?\n/)) {
    const line = rawLine.trim();
    if (!line || line.startsWith("#")) continue;
    const colon = line.indexOf(":");
    if (colon <= 0) throw new Error(`bad handler line (expected "kind:command"): "${line}"`);
    const kind = normalizeEventKind(line.slice(0, colon));
    const command = line.slice(colon + 1).trim();
    if (!command) throw new Error(`empty command for kind "${kind}"`);
    out.push({ eventKind: kind, command });
  }
  if (out.length === 0) throw new Error("provide at least one handler (kind:command per line)");
  return out;
}

export function initState(config: WitnessesConfig, startPrice: number): WitnessesState {
  const byEventKind: Record<string, number> = {};
  for (const h of config.handlers) byEventKind[h.eventKind] = 0;
  return {
    config,
    status: "monitoring",
    currentPrice: startPrice,
    invocations: [],
    stats: { triggered: 0, succeeded: 0, failed: 0, byEventKind },
  };
}

/** Applies one tick. Returns state + emitted log events. */
export function step(
  state: WitnessesState,
  price: number,
  now: number,
): { state: WitnessesState; events: string[] } {
  if (state.status !== "monitoring") return { state, events: [] };
  const events: string[] = [];
  state.currentPrice = price;

  // Simulate at most one event per tick (Poisson-ish thin approximation).
  const arrivalProb = clamp(state.config.eventArrivalPerTick, 0, 1);
  if (Math.random() >= arrivalProb) return { state, events };

  // Pick an event kind uniformly from EVENT_POOL (mirrors real ledger stream
  // where any subscribed type may arrive).
  const kind = EVENT_POOL[Math.floor(Math.random() * EVENT_POOL.length)];

  // Match to handler (case-insensitive on the normalized kind).
  const handler = state.config.handlers.find((h) => h.eventKind === kind);
  if (!handler) return { state, events };

  // Build the env block just like spawn_command() in Rust.
  const env: Record<string, string> = {
    SILVANA_EVENT: kind,
    SILVANA_EVENT_TS: new Date(now).toISOString(),
    SILVANA_MARKET_ID: state.config.market || randomMarket(),
  };
  if (kind.startsWith("settlement.")) {
    env.SILVANA_PROPOSAL_ID = randomId("prop");
    env.SILVANA_STATUS = kind.slice("settlement.".length).toUpperCase();
  } else if (kind.startsWith("order.")) {
    env.SILVANA_ORDER_ID = randomId("ord");
  }

  // Simulate command execution: duration with ±40% jitter, exit code from failure rate.
  const jitter = 1 + (Math.random() - 0.5) * 0.8;
  const durationMs = Math.max(1, Math.round(state.config.commandDurationMs * jitter));
  const failed = Math.random() < clamp(state.config.commandFailureRate, 0, 1);
  const exitCode = failed ? (Math.random() < 0.5 ? 1 : 127) : 0;
  const stdout = failed
    ? `error: handler for ${kind} exited ${exitCode}`
    : `ok: ${kind} handled in ${durationMs}ms`;

  const seq = state.stats.triggered + 1;
  const invocation: CommandInvocation = {
    seq,
    t: now,
    eventKind: kind,
    command: handler.command,
    exitCode,
    stdout,
    durationMs,
    env,
  };
  state.invocations.push(invocation);
  if (state.invocations.length > MAX_INVOCATIONS) state.invocations.shift();
  state.stats.triggered = seq;
  state.stats.byEventKind[kind] = (state.stats.byEventKind[kind] ?? 0) + 1;
  if (failed) state.stats.failed += 1;
  else state.stats.succeeded += 1;

  events.push(`TRIGGER [${kind}] -> ${handler.command} exit=${exitCode}`);
  events.push(`EXEC ${handler.command} exit=${exitCode} duration=${durationMs}ms`);
  return { state, events };
}

function clamp(n: number, lo: number, hi: number): number {
  return Math.max(lo, Math.min(hi, n));
}

function randomId(prefix: string): string {
  const hex = Math.floor(Math.random() * 0xffffff).toString(16).padStart(6, "0");
  return `${prefix}-${hex}`;
}

const FAKE_MARKETS = ["CC-USDC", "BTC-USD", "CETH-CC", "CBTC-CC"];
function randomMarket(): string {
  return FAKE_MARKETS[Math.floor(Math.random() * FAKE_MARKETS.length)];
}
