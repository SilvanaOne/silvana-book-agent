// In-memory demo store for agent-batch-order-management.
//
// Holds the seeded "Development portfolio" (positions + targets), the current
// rebalance plan (from preview), and at most one in-flight rebalance job that
// animates deterministically through queued → running → completed based on
// elapsed wall-clock time. A 1s tick loop advances the job, applies an optional
// gentle price walk, and records audit/events. No network, no DB, no Redis.

import {
  analyzeDrift,
  planOrders,
  planTransfers,
  targetAlignedPositions,
  mockTxHash,
  DEFAULT_THRESHOLD_BPS,
  QUOTE_CURRENCY,
  SEED_POSITIONS,
  SEED_TARGETS,
  type DriftAnalysis,
  type PlannedOrder,
  type Position,
  type Target,
  type Transfer,
} from "./rebalance-engine";
import { nextPrice, type WalkParams } from "./price-simulator";

export type JobPhase = "queued" | "running" | "completed";
export type StepStatus = "pending" | "executing" | "done";

export type JobStep = Readonly<{ transfer: Transfer; status: StepStatus }>;

export type RebalanceJob = {
  id: string;
  createdAt: number;
  startedAt: number | null;
  completedAt: number | null;
  phase: JobPhase;
  mode: "plan_only";
  thresholdBps: number;
  nav: number;
  orders: PlannedOrder[];
  steps: JobStep[];
  progressPct: number;
};

export type RebalancePlan = Readonly<{
  createdAt: number;
  nav: number;
  thresholdBps: number;
  orders: PlannedOrder[];
  transfers: Transfer[];
  estimatedNotional: number;
}>;

export type EventEntry = Readonly<{ t: number; message: string }>;
export type AuditEntry = Readonly<{ t: number; action: string; hash: string }>;

type StoreState = {
  positions: Position[];
  targets: Target[];
  thresholdBps: number;
  plan: RebalancePlan | null;
  job: RebalanceJob | null;
  events: EventEntry[];
  audit: AuditEntry[];
  walk: WalkParams;
  timer: ReturnType<typeof setInterval> | null;
};

// Deterministic, time-based job timeline.
const QUEUE_MS = 1600; // sit in the queue before the worker picks it up
const STEP_MS = 1000; // each settlement transfer clears in ~1 tick
const FINALIZE_MS = 800; // settle-to-targets + audit after the last transfer

const MAX_EVENTS = 120;
const MAX_AUDIT = 60;

const g = globalThis as unknown as { __batchOrderMgmtStore?: StoreState };

function clonePositions(src: readonly Position[]): Position[] {
  return src.map((p) => ({ ...p }));
}
function cloneTargets(src: readonly Target[]): Target[] {
  return src.map((t) => ({ ...t }));
}

function getStore(): StoreState {
  if (!g.__batchOrderMgmtStore) {
    g.__batchOrderMgmtStore = {
      positions: clonePositions(SEED_POSITIONS),
      targets: cloneTargets(SEED_TARGETS),
      thresholdBps: DEFAULT_THRESHOLD_BPS,
      plan: null,
      job: null,
      events: [
        { t: Date.now(), message: `Loaded "Development portfolio" — 4 targets, threshold ±${DEFAULT_THRESHOLD_BPS} bps` },
      ],
      audit: [],
      walk: { driftPerTick: 0, volPerTick: 0 },
      timer: null,
    };
    ensureTimer(g.__batchOrderMgmtStore);
  }
  return g.__batchOrderMgmtStore;
}

function pushEvent(store: StoreState, message: string): void {
  store.events.push({ t: Date.now(), message });
  while (store.events.length > MAX_EVENTS) store.events.shift();
}
function pushAudit(store: StoreState, action: string): void {
  store.audit.push({ t: Date.now(), action, hash: mockTxHash() });
  while (store.audit.length > MAX_AUDIT) store.audit.shift();
}

function currentAnalysis(store: StoreState): DriftAnalysis {
  return analyzeDrift(store.positions, store.targets, store.thresholdBps, QUOTE_CURRENCY);
}

/** Build (or rebuild) the rebalance plan from live drift and stash it. */
export function previewRebalance(): RebalancePlan {
  const store = getStore();
  const analysis = currentAnalysis(store);
  const orders = planOrders(analysis);
  const transfers = planTransfers(orders, QUOTE_CURRENCY);
  const estimatedNotional = orders.reduce((s, o) => s + o.notional, 0);
  const plan: RebalancePlan = {
    createdAt: Date.now(),
    nav: analysis.nav,
    thresholdBps: store.thresholdBps,
    orders,
    transfers,
    estimatedNotional,
  };
  store.plan = plan;
  pushEvent(store, `PREVIEW: ${orders.length} order(s), notional ${estimatedNotional.toFixed(2)} ${QUOTE_CURRENCY}, ${analysis.breaches} breach(es)`);
  pushAudit(store, "portfolio.rebalance.preview");
  return plan;
}

/** Kick off a rebalance job from the current (or freshly computed) plan. */
export function executeRebalance(): RebalanceJob | { error: string } {
  const store = getStore();
  if (store.job && store.job.phase !== "completed") {
    return { error: "a rebalance job is already in flight" };
  }
  const plan = store.plan ?? previewRebalance();
  if (plan.orders.length === 0) {
    return { error: "portfolio is within band — nothing to rebalance" };
  }
  const now = Date.now();
  const job: RebalanceJob = {
    id: `job-${now.toString(36)}`,
    createdAt: now,
    startedAt: null,
    completedAt: null,
    phase: "queued",
    mode: "plan_only",
    thresholdBps: plan.thresholdBps,
    nav: plan.nav,
    orders: plan.orders.map((o) => ({ ...o })),
    steps: plan.transfers.map((transfer) => ({ transfer, status: "pending" as StepStatus })),
    progressPct: 0,
  };
  store.job = job;
  pushEvent(store, `EXECUTE ${job.id}: queued ${job.steps.length} transfer(s), NAV ${job.nav.toFixed(2)} ${QUOTE_CURRENCY}`);
  pushAudit(store, "portfolio.rebalance.execute");
  ensureTimer(store);
  return job;
}

export function resetPortfolio(): void {
  const store = getStore();
  store.positions = clonePositions(SEED_POSITIONS);
  store.targets = cloneTargets(SEED_TARGETS);
  store.thresholdBps = DEFAULT_THRESHOLD_BPS;
  store.plan = null;
  store.job = null;
  store.walk = { driftPerTick: 0, volPerTick: 0 };
  pushEvent(store, "RESET: portfolio restored to seed snapshot");
  pushAudit(store, "portfolio.reset");
}

export function setThresholdBps(bps: number): void {
  const store = getStore();
  store.thresholdBps = Math.max(0, bps);
  store.plan = null; // stale — force a re-preview
  pushEvent(store, `THRESHOLD set to ±${store.thresholdBps} bps`);
}

export function updateWalk(patch: Partial<WalkParams>): void {
  const store = getStore();
  store.walk = { ...store.walk, ...patch };
}

/**
 * Imitate drift: shove one asset's weight off target by `driftWeight` (decimal
 * share, e.g. +0.15). Remaining weight is redistributed across the other enabled
 * assets pro-rata to their target weights, holding NAV constant. Mirrors the
 * apps/api imitate-drift demo tool.
 */
export function imitateDrift(assetSymbol: string, driftWeight: number): { ok: true } | { error: string } {
  const store = getStore();
  const enabled = store.targets.filter((t) => t.enabled);
  if (enabled.length < 2) return { error: "need at least 2 enabled targets" };
  const tgt = enabled.find((t) => t.assetSymbol === assetSymbol);
  if (!tgt) return { error: `${assetSymbol} is not an enabled target` };
  const others = enabled.filter((t) => t.assetSymbol !== assetSymbol);
  const othersTargetSum = others.reduce((s, t) => s + t.targetWeight, 0);
  if (othersTargetSum <= 0) return { error: "counter-assets have zero target weight" };

  const nav = store.positions.reduce((s, p) => s + p.qty * p.price, 0);
  if (nav <= 0) return { error: "NAV must be positive" };

  const newActualW = tgt.targetWeight + driftWeight;
  if (newActualW < 0 || newActualW > 1) return { error: `resulting weight ${newActualW.toFixed(3)} out of [0,1]` };
  const remainder = 1 - newActualW;

  const priceBySym = new Map(store.positions.map((p) => [p.assetSymbol, p.price] as const));
  const setQty = (sym: string, weight: number) => {
    const price = priceBySym.get(sym) ?? 1;
    const mv = weight * nav;
    const pos = store.positions.find((p) => p.assetSymbol === sym);
    if (pos) pos.qty = price > 0 ? mv / price : 0;
  };

  setQty(tgt.assetSymbol, newActualW);
  for (const o of others) setQty(o.assetSymbol, remainder * (o.targetWeight / othersTargetSum));

  store.plan = null;
  pushEvent(store, `DRIFT imitated: ${assetSymbol} → ${(newActualW * 100).toFixed(1)}% (${driftWeight >= 0 ? "+" : ""}${(driftWeight * 100).toFixed(1)}pp)`);
  pushAudit(store, "portfolio.drift.imitated");
  return { ok: true };
}

export type Snapshot = ReturnType<typeof snapshot>;

export function snapshot() {
  const store = getStore();
  const analysis = currentAnalysis(store);
  return {
    portfolio: {
      id: "00000000-0000-4000-a000-000000000001",
      name: "Development portfolio",
      quoteCurrency: QUOTE_CURRENCY,
    },
    analysis,
    targets: store.targets,
    plan: store.plan,
    job: store.job,
    events: store.events,
    audit: store.audit,
    walk: store.walk,
  };
}

// --- tick loop ------------------------------------------------------------

function ensureTimer(store: StoreState): void {
  if (store.timer) return;
  store.timer = setInterval(() => tick(store), 1000);
}

function tick(store: StoreState): void {
  // 1) optional gentle price walk (USDC is the stable quote — pinned to 1).
  if (store.walk.volPerTick > 0 || store.walk.driftPerTick !== 0) {
    for (const p of store.positions) {
      if (p.assetSymbol === QUOTE_CURRENCY) continue;
      p.price = nextPrice(p.price, store.walk);
    }
  }

  // 2) advance the in-flight job deterministically from elapsed time.
  const job = store.job;
  if (!job || job.phase === "completed") return;

  const now = Date.now();
  const elapsed = now - job.createdAt;
  const stepCount = job.steps.length;

  if (elapsed < QUEUE_MS) {
    job.phase = "queued";
    job.progressPct = Math.min(8, Math.round((elapsed / QUEUE_MS) * 8));
    return;
  }

  const runElapsed = elapsed - QUEUE_MS;
  if (job.startedAt === null) {
    job.startedAt = job.createdAt + QUEUE_MS;
    job.phase = "running";
    pushEvent(store, `RUNNING ${job.id}: worker picked up ${stepCount} transfer(s)`);
  }

  const doneCount = Math.min(stepCount, Math.floor(runElapsed / STEP_MS));
  const executingIdx = doneCount < stepCount ? doneCount : -1;
  for (let i = 0; i < stepCount; i++) {
    const prev = job.steps[i].status;
    const status: StepStatus = i < doneCount ? "done" : i === executingIdx ? "executing" : "pending";
    if (status !== prev) {
      job.steps = job.steps.map((s, idx) => (idx === i ? { ...s, status } : s));
      if (status === "done") {
        const tr = job.steps[i].transfer;
        pushEvent(store, `TRANSFER ${i + 1}/${stepCount} settled: ${tr.from} → ${tr.to} on ${tr.venueId} (${tr.txHash.slice(0, 10)}…)`);
      }
    }
  }

  const workDone = runElapsed >= stepCount * STEP_MS + FINALIZE_MS;
  const settleProgress = Math.min(1, runElapsed / (stepCount * STEP_MS));
  job.progressPct = workDone ? 100 : Math.min(96, 8 + Math.round(settleProgress * 88));

  if (workDone && job.phase === "running") {
    // Close the loop: snapshot the portfolio to target weights (drift → ~0).
    store.positions = targetAlignedPositions(store.positions, store.targets, QUOTE_CURRENCY);
    job.completedAt = now;
    job.phase = "completed";
    job.progressPct = 100;
    store.plan = null;
    pushEvent(store, `COMPLETED ${job.id}: portfolio aligned to targets, drift ~0`);
    pushAudit(store, "portfolio.rebalance.completed");
  }
}
