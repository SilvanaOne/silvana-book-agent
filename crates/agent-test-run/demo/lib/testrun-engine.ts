// Port of agent-test-run to TypeScript. Mirrors
// crates/agent-test-run/src/main.rs (fn validate) — runs a sequence of
// read-only RPC probes sequentially, each recording a pass/fail + latency.
//
// This demo doesn't hit a real orderbook; each check is simulated with a
// configurable base latency + failure-probability so an operator can
// watch a green-then-red run cycle.

export type CheckId =
  | "connect+jwt"
  | "get_markets"
  | "get_active_orders"
  | "get_pending_proposals"
  | "get_market_data"
  | "get_price"
  | "get_orderbook_depth";

export const ALL_CHECKS: readonly CheckId[] = [
  "connect+jwt",
  "get_markets",
  "get_active_orders",
  "get_pending_proposals",
  "get_market_data",
  "get_price",
  "get_orderbook_depth",
] as const;

// Per-check base latency (ms) — realistic ranges for read-only gRPC on devnet.
const BASE_LATENCY_MS: Record<CheckId, [number, number]> = {
  "connect+jwt": [80, 220],
  "get_markets": [30, 90],
  "get_active_orders": [40, 120],
  "get_pending_proposals": [40, 120],
  "get_market_data": [50, 150],
  "get_price": [60, 180],
  "get_orderbook_depth": [70, 200],
};

export type TestRunConfig = Readonly<{
  partyId: string;
  endpoint: string;
  market: string;                    // "" means skip per-market checks
  failureRate: number;               // 0..1 — per-check failure probability
  latencyMultiplier: number;         // 0.5..5 multiplier on base latency
  runIntervalSecs: number;           // seconds between full runs; 0 = one-shot
}>;

export type CheckResult = Readonly<{
  id: CheckId;
  status: "pass" | "fail" | "skipped" | "pending";
  latencyMs: number;
  detail: string;
  finishedAt: number | null;
}>;

export type Run = Readonly<{
  seq: number;
  startedAt: number;
  finishedAt: number | null;
  totalMs: number;
  passed: number;
  failed: number;
  skipped: number;
  results: CheckResult[];
}>;

export type TestRunState = {
  config: TestRunConfig;
  status: "running" | "idle";
  currentRun: Run | null;             // in-flight run
  lastRun: Run | null;
  runsCompleted: number;
  totalPassed: number;
  totalFailed: number;
  totalSkipped: number;
  nextRunAt: number | null;
  history: Run[];                     // bounded
};

const MAX_HISTORY = 16;

export function initState(config: TestRunConfig, now: number): TestRunState {
  const run = startRun(1, now, config);
  return {
    config,
    status: "running",
    currentRun: run,
    lastRun: null,
    runsCompleted: 0,
    totalPassed: 0,
    totalFailed: 0,
    totalSkipped: 0,
    nextRunAt: null,
    history: [],
  };
}

/** Advance one tick. `_price` is unused — kept for store timer symmetry. */
export function step(state: TestRunState, _price: number, now: number): { state: TestRunState; events: string[] } {
  if (state.status !== "running") return { state, events: [] };
  const events: string[] = [];

  // If a run is in flight, advance one check per tick until finished.
  if (state.currentRun) {
    const run = state.currentRun;
    const nextIdx = run.results.findIndex((r) => r.status === "pending");
    if (nextIdx >= 0) {
      const id = run.results[nextIdx].id;
      const skip = shouldSkip(id, state.config);
      let result: CheckResult;
      if (skip) {
        result = { id, status: "skipped", latencyMs: 0, detail: "no market provided", finishedAt: now };
        events.push(`SKIP  [${id}]  no market provided`);
      } else {
        const [lo, hi] = BASE_LATENCY_MS[id];
        const latency = Math.round((lo + Math.random() * (hi - lo)) * state.config.latencyMultiplier);
        const fail = Math.random() < state.config.failureRate;
        if (fail) {
          result = { id, status: "fail", latencyMs: latency, detail: failReason(id), finishedAt: now };
          events.push(`FAIL  [${id}]  ${result.detail} (${latency} ms)`);
        } else {
          const detail = successDetail(id, state.config);
          result = { id, status: "pass", latencyMs: latency, detail, finishedAt: now };
          events.push(`PASS  [${id}]  ${detail} (${latency} ms)`);
        }
      }
      const results = run.results.slice();
      results[nextIdx] = result;
      const passed = results.filter((r) => r.status === "pass").length;
      const failed = results.filter((r) => r.status === "fail").length;
      const skipped = results.filter((r) => r.status === "skipped").length;
      state.currentRun = { ...run, results, passed, failed, skipped };
    } else {
      // Finalise the run.
      const finalRun: Run = { ...run, finishedAt: now, totalMs: now - run.startedAt };
      state.lastRun = finalRun;
      state.history.push(finalRun);
      while (state.history.length > MAX_HISTORY) state.history.shift();
      state.runsCompleted += 1;
      state.totalPassed += finalRun.passed;
      state.totalFailed += finalRun.failed;
      state.totalSkipped += finalRun.skipped;
      state.currentRun = null;
      events.push(`DONE  run=${finalRun.seq}  pass=${finalRun.passed} fail=${finalRun.failed} skip=${finalRun.skipped} in ${finalRun.totalMs} ms`);
      if (state.config.runIntervalSecs > 0) {
        state.nextRunAt = now + state.config.runIntervalSecs * 1000;
      } else {
        state.status = "idle";
        events.push("Idle — one-shot mode; no further runs scheduled");
      }
    }
  } else if (state.nextRunAt !== null && now >= state.nextRunAt) {
    state.currentRun = startRun(state.runsCompleted + 1, now, state.config);
    state.nextRunAt = null;
    events.push(`Starting run #${state.currentRun.seq}`);
  }

  return { state, events };
}

function startRun(seq: number, now: number, _config: TestRunConfig): Run {
  return {
    seq,
    startedAt: now,
    finishedAt: null,
    totalMs: 0,
    passed: 0,
    failed: 0,
    skipped: 0,
    results: ALL_CHECKS.map((id) => ({ id, status: "pending" as const, latencyMs: 0, detail: "", finishedAt: null })),
  };
}

function shouldSkip(id: CheckId, cfg: TestRunConfig): boolean {
  if ((id === "get_price" || id === "get_orderbook_depth") && cfg.market.trim().length === 0) return true;
  return false;
}

function successDetail(id: CheckId, cfg: TestRunConfig): string {
  switch (id) {
    case "connect+jwt": return `connected as ${cfg.partyId}`;
    case "get_markets": return `${3 + Math.floor(Math.random() * 6)} markets`;
    case "get_active_orders": return `${Math.floor(Math.random() * 20)} orders`;
    case "get_pending_proposals": return `${Math.floor(Math.random() * 5)} pending`;
    case "get_market_data": return `${5 + Math.floor(Math.random() * 15)} entries`;
    case "get_price": return `last=${(0.14 + Math.random() * 0.02).toFixed(6)} src=silvana.oracle`;
    case "get_orderbook_depth": return `bids=5 offers=5`;
  }
}

function failReason(id: CheckId): string {
  const reasons: Record<CheckId, string[]> = {
    "connect+jwt": ["tls handshake timeout", "JWT rejected: signature mismatch", "grpc UNAVAILABLE"],
    "get_markets": ["rpc timeout after 5000 ms", "grpc INTERNAL"],
    "get_active_orders": ["JWT scope missing party:read", "grpc DEADLINE_EXCEEDED"],
    "get_pending_proposals": ["settlement service down", "grpc UNAVAILABLE"],
    "get_market_data": ["pricing feed disconnected"],
    "get_price": ["market not found", "external feed timeout"],
    "get_orderbook_depth": ["market not found", "empty book"],
  };
  const arr = reasons[id];
  return arr[Math.floor(Math.random() * arr.length)];
}
