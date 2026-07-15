/**
 * Standalone in-process "backend" for the demo build.
 *
 * The real web app talks to `apps/api` (Express + Prisma + Redis) over HTTP. In this
 * demo there is NO backend, DB, queue or network: every request that the real app would
 * send to `apps/api` is resolved here, in-process, against synthetic fixtures derived
 * from `prisma/seed.ts`. Both server-side chokepoints (`lib/backend.ts` `backendJson`
 * and `lib/proxy-backend.ts` `forwardBackend`) funnel through {@link handleBackend}.
 *
 * Mutable demo state lives on `globalThis` so that the React-Server-Components module
 * layer and the route-handler module layer (which Webpack may instantiate separately)
 * observe the exact same portfolio / jobs / audit log — one Node process, one global.
 *
 * The rebalance math mirrors `packages/portfolio-engine` (`analyzeRebalance` +
 * `resolveTargetWeights`) closely enough that drift, plans and previews behave like the
 * real API; numbers are formatted as decimal strings to match the wire shapes.
 */

import { DEV_PORTFOLIO_ID } from "./dev-portfolio";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

type DemoTarget = {
  assetSymbol: string;
  targetWeight: number;
  minWeight: number | null;
  maxWeight: number | null;
  enabled: boolean;
};

type DemoPosition = {
  assetSymbol: string;
  qty: number;
  price: number;
  marketValue: number;
};

type PlannedOrder = {
  market: string;
  side: "buy" | "sell";
  type: "limit";
  qty: string;
  price: string;
  assetSymbol: string;
};

type DemoJob = {
  jobId: string;
  portfolioId: string;
  mode: string; // "dry_run_execute" | "live_execute"
  dryRun: boolean;
  requestedBy: string | null;
  createdAtMs: number;
  input: unknown;
  plannedOrders: PlannedOrder[];
  nav: string;
  estimatedNotional: string;
};

type AuditEntry = {
  id: string;
  actorId: string;
  action: string;
  entityType: string;
  entityId: string;
  payload: unknown;
  createdAt: string; // ISO
};

type DemoState = {
  portfolio: { portfolioId: string; name: string; baseCurrency: string };
  targets: DemoTarget[];
  positions: DemoPosition[];
  jobs: Map<string, DemoJob>;
  audit: AuditEntry[]; // newest last
};

type BackendResult = { status: number; json: unknown };

// ---------------------------------------------------------------------------
// State (seeded from prisma/seed.ts), persisted on globalThis
// ---------------------------------------------------------------------------

const STATE_KEY = "__batchOrderDemoState__";

function seedState(): DemoState {
  const nowIso = new Date().toISOString();
  const t = (msAgo: number) => new Date(Date.now() - msAgo).toISOString();

  return {
    portfolio: { portfolioId: DEV_PORTFOLIO_ID, name: "Development portfolio", baseCurrency: "USDC" },
    targets: [
      { assetSymbol: "WBTC", targetWeight: 0.25, minWeight: null, maxWeight: null, enabled: true },
      { assetSymbol: "WETH", targetWeight: 0.25, minWeight: null, maxWeight: null, enabled: true },
      { assetSymbol: "CC", targetWeight: 0.1, minWeight: null, maxWeight: null, enabled: true },
      { assetSymbol: "USDC", targetWeight: 0.4, minWeight: 0.1, maxWeight: 0.9, enabled: true },
    ],
    positions: [
      { assetSymbol: "WBTC", qty: 0.05, price: 100000, marketValue: 5000 },
      { assetSymbol: "WETH", qty: 1.5, price: 3500, marketValue: 5250 },
      { assetSymbol: "CC", qty: 1000, price: 0.156, marketValue: 156 },
      { assetSymbol: "USDC", qty: 10000, price: 1, marketValue: 10000 },
    ],
    jobs: new Map(),
    audit: [
      {
        id: crypto.randomUUID(),
        actorId: "system",
        action: "portfolio.seeded",
        entityType: "portfolio",
        entityId: DEV_PORTFOLIO_ID,
        payload: { name: "Development portfolio", baseCurrency: "USDC", assets: ["WBTC", "WETH", "CC", "USDC"] },
        createdAt: t(1000 * 60 * 42),
      },
      {
        id: crypto.randomUUID(),
        actorId: "user:operator",
        action: "portfolio.targets.updated",
        entityType: "portfolio",
        entityId: DEV_PORTFOLIO_ID,
        payload: {
          before: [
            { assetSymbol: "WBTC", targetWeight: "0.3", enabled: true },
            { assetSymbol: "WETH", targetWeight: "0.3", enabled: true },
            { assetSymbol: "CC", targetWeight: "0.1", enabled: true },
            { assetSymbol: "USDC", targetWeight: "0.3", enabled: true },
          ],
          after: [
            { assetSymbol: "WBTC", targetWeight: "0.25", minWeight: null, maxWeight: null, enabled: true },
            { assetSymbol: "WETH", targetWeight: "0.25", minWeight: null, maxWeight: null, enabled: true },
            { assetSymbol: "CC", targetWeight: "0.1", minWeight: null, maxWeight: null, enabled: true },
            { assetSymbol: "USDC", targetWeight: "0.4", minWeight: "0.1", maxWeight: "0.9", enabled: true },
          ],
        },
        createdAt: t(1000 * 60 * 30),
      },
      {
        id: crypto.randomUUID(),
        actorId: "user:operator",
        action: "rebalance.preview",
        entityType: "rebalance_job",
        entityId: crypto.randomUUID(),
        payload: { portfolioId: DEV_PORTFOLIO_ID },
        createdAt: t(1000 * 60 * 12),
      },
      {
        id: crypto.randomUUID(),
        actorId: "system",
        action: "position.snapshot.ingested",
        entityType: "portfolio",
        entityId: DEV_PORTFOLIO_ID,
        payload: { nav: "20406", asOf: nowIso, source: "pricing_stream_demo" },
        createdAt: t(1000 * 60 * 5),
      },
    ],
  };
}

function state(): DemoState {
  const g = globalThis as unknown as Record<string, DemoState | undefined>;
  let s = g[STATE_KEY];
  if (!s) {
    s = seedState();
    g[STATE_KEY] = s;
  }
  return s;
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;

function isUuid(v: unknown): v is string {
  return typeof v === "string" && UUID_RE.test(v);
}

/** Decimal-like string: no exponential notation, IEEE-754 tail noise stripped, zeros trimmed. */
function dstr(n: number): string {
  if (!Number.isFinite(n)) return "0";
  if (Number.isInteger(n)) return String(n);
  // Drop the float rounding tail the real Decimal engine would not surface (e.g. 1884.6000000000001).
  let s = String(Number(n.toPrecision(15)));
  if (s.includes("e") || s.includes("E")) {
    s = n.toFixed(18).replace(/0+$/, "").replace(/\.$/, "");
  }
  return s;
}

function floorDp(n: number, dp: number): number {
  const f = Math.pow(10, dp);
  return Math.floor(n * f) / f;
}

function ok(json: unknown): BackendResult {
  return { status: 200, json };
}

function err(status: number, json: unknown): BackendResult {
  return { status, json };
}

function randomHex(bytes: number): string {
  const arr = new Uint8Array(bytes);
  crypto.getRandomValues(arr);
  let out = "";
  for (const b of arr) out += b.toString(16).padStart(2, "0");
  return out;
}

function mockTxHash(): string {
  return "0x" + randomHex(32);
}

function appendAudit(entry: Omit<AuditEntry, "id" | "createdAt">): string {
  const id = crypto.randomUUID();
  state().audit.push({ ...entry, id, createdAt: new Date().toISOString() });
  return id;
}

// ---------------------------------------------------------------------------
// Rebalance engine (port of packages/portfolio-engine analyzeRebalance)
// ---------------------------------------------------------------------------

type TargetInput = { assetSymbol: string; weight: number; enabled?: boolean };

type AnalyzeResult = {
  portfolioId: string;
  quoteCurrency: string;
  nav: string;
  normalizedTargets: Array<{ assetSymbol: string; weight: string }>;
  current: Array<{ assetSymbol: string; weight: string }>;
  drift: Array<{
    assetSymbol: string;
    targetWeight: string;
    currentWeight: string;
    driftWeightBps: string;
    skippedDueToThreshold: boolean;
  }>;
  plannedOrders: PlannedOrder[];
  estimatedNotional: string;
  warnings: string[];
};

/** Mirrors resolveTargetWeights: normalize enabled targets, inject implicit quote if absent. */
function resolveTargetWeights(
  targets: TargetInput[],
  quote: string,
): { weights: Map<string, number>; warnings: string[] } {
  const warnings: string[] = [];
  const raw = new Map<string, number>();
  for (const row of targets) {
    if (row.enabled === false) continue;
    const sym = row.assetSymbol.trim();
    if (!sym) throw new Error("target.assetSymbol must be non-empty");
    if (raw.has(sym)) throw new Error(`duplicate target for ${sym}`);
    const w = row.weight;
    if (w < 0 || w > 1) throw new Error(`target weight for ${sym} out of range [0,1]: ${dstr(w)}`);
    raw.set(sym, w);
  }
  if (raw.size === 0) throw new Error("no active targets");

  if (raw.has(quote)) {
    let sumAll = 0;
    for (const w of raw.values()) sumAll += w;
    if (Math.abs(sumAll - 1) > 1e-9) throw new Error(`target weights must sum to 1, got ${dstr(sumAll)}`);
    return { weights: raw, warnings };
  }

  let sumRisky = 0;
  for (const [sym, w] of raw) if (sym !== quote) sumRisky += w;
  if (sumRisky > 1 + 1e-9) throw new Error(`sum of non-quote targets exceeds 1: ${dstr(sumRisky)}`);
  const quoteW = 1 - sumRisky;
  raw.set(quote, quoteW);
  warnings.push(`implicit quote target for ${quote} = ${dstr(quoteW)} (1 − sum(other targets))`);
  return { weights: raw, warnings };
}

function analyzeRebalance(params: {
  portfolioId: string;
  quoteCurrency: string;
  thresholdBps: number;
  targets: TargetInput[];
  positions: DemoPosition[];
  slipBpsBuy?: number;
  slipBpsSell?: number;
}): AnalyzeResult {
  const quote = params.quoteCurrency.trim();
  if (!quote) throw new Error("quoteCurrency required");
  const slipBuy = params.slipBpsBuy ?? 0;
  const slipSell = params.slipBpsSell ?? 0;

  const { weights: targetMap, warnings: wTargets } = resolveTargetWeights(params.targets, quote);
  const targetKeys = new Set(targetMap.keys());

  const posMap = new Map<string, DemoPosition>();
  for (const p of params.positions) {
    if (posMap.has(p.assetSymbol)) throw new Error(`duplicate position for ${p.assetSymbol}`);
    if (p.price <= 0) throw new Error(`non-positive price for ${p.assetSymbol}`);
    posMap.set(p.assetSymbol, p);
  }

  // validatePositionUniverse: every position must map to a target.
  for (const sym of posMap.keys()) {
    if (!targetKeys.has(sym)) {
      throw new Error(
        `position for ${sym} is not in targets (add target or remove position). Quote is ${quote}.`,
      );
    }
  }

  const warnings = [...wTargets];
  for (const sym of targetKeys) {
    if (!posMap.has(sym)) warnings.push(`no position row for ${sym}; market value assumed 0`);
  }

  let nav = 0;
  for (const sym of targetKeys) nav += posMap.get(sym)?.marketValue ?? 0;
  if (nav <= 0) throw new Error("NAV must be positive");

  const threshold = params.thresholdBps / 10000;
  if (threshold < 0) throw new Error("thresholdBps must be non-negative");

  const normalizedTargets: AnalyzeResult["normalizedTargets"] = [];
  const currentOut: AnalyzeResult["current"] = [];
  const drift: AnalyzeResult["drift"] = [];
  const planned: PlannedOrder[] = [];
  let estimatedNotional = 0;

  const sortedSyms = [...targetKeys].sort();
  for (const sym of sortedSyms) {
    const tw = targetMap.get(sym) ?? 0;
    const mv = posMap.get(sym)?.marketValue ?? 0;
    const cw = mv / nav;

    normalizedTargets.push({ assetSymbol: sym, weight: dstr(tw) });
    currentOut.push({ assetSymbol: sym, weight: dstr(cw) });

    const delta = tw - cw;
    const absDelta = Math.abs(delta);
    const skipped = absDelta < threshold;

    drift.push({
      assetSymbol: sym,
      targetWeight: dstr(tw),
      currentWeight: dstr(cw),
      driftWeightBps: (delta * 10000).toFixed(4),
      skippedDueToThreshold: skipped,
    });

    if (sym === quote) continue;
    if (skipped) continue;

    const row = posMap.get(sym);
    if (!row) throw new Error(`cannot plan trade without position/mark price row for asset ${sym}`);
    const mid = row.price;
    if (mid <= 0) throw new Error(`invalid mid price for ${sym}`);

    const notional = absDelta * nav;
    const side: "buy" | "sell" = delta > 0 ? "buy" : "sell";
    const slip = side === "buy" ? slipBuy : slipSell;
    const limitPx = slip <= 0 ? mid : side === "buy" ? mid * (1 + slip / 10000) : mid * (1 - slip / 10000);
    const qty = floorDp(notional / limitPx, 12);
    if (qty <= 0) continue;

    estimatedNotional += notional;
    planned.push({
      market: `${sym}-${quote}`,
      side,
      type: "limit",
      qty: dstr(qty),
      price: dstr(limitPx),
      assetSymbol: sym,
    });
  }

  return {
    portfolioId: params.portfolioId,
    quoteCurrency: quote,
    nav: dstr(nav),
    normalizedTargets,
    current: currentOut,
    drift,
    plannedOrders: planned,
    estimatedNotional: dstr(estimatedNotional),
    warnings,
  };
}

function targetsFromPortfolio(): TargetInput[] {
  return state()
    .targets.filter((t) => t.enabled)
    .map((t) => ({ assetSymbol: t.assetSymbol, weight: t.targetWeight, enabled: true }));
}

function maxAbsDriftBps(drift: AnalyzeResult["drift"]): number {
  let m = 0;
  for (const d of drift) {
    const v = Number(d.driftWeightBps);
    if (Number.isFinite(v)) {
      const a = Math.abs(Math.round(v));
      if (a > m) m = a;
    }
  }
  return m;
}

// ---------------------------------------------------------------------------
// Route handlers
// ---------------------------------------------------------------------------

const DEFAULT_THRESHOLD_BPS = 100; // REBALANCE_THRESHOLD_BPS default
const SLIP_BPS = 50; // RISK_MAX_SLIPPAGE_BPS default

function parseBody(bodyText?: string): Record<string, unknown> {
  if (!bodyText || bodyText.trim() === "") return {};
  try {
    const v = JSON.parse(bodyText);
    return typeof v === "object" && v !== null ? (v as Record<string, unknown>) : {};
  } catch {
    return {};
  }
}

function decodeTargets(bodyTargets: unknown): TargetInput[] | null {
  if (bodyTargets === undefined || bodyTargets === null) return null;
  if (!Array.isArray(bodyTargets)) throw new Error("targets must be an array");
  return bodyTargets.map((row, idx) => {
    if (!row || typeof row !== "object") throw new Error(`targets[${idx}] invalid`);
    const r = row as Record<string, unknown>;
    const assetSymbol = typeof r.assetSymbol === "string" ? r.assetSymbol.trim() : String(r.assetSymbol ?? "").trim();
    if (!assetSymbol) throw new Error(`targets[${idx}].assetSymbol required`);
    if (typeof r.weight !== "number" && typeof r.weight !== "string") throw new Error(`targets[${idx}].weight invalid`);
    const w = typeof r.weight === "number" ? r.weight : Number(r.weight);
    if (!Number.isFinite(w) || w < 0 || w > 1) throw new Error(`targets[${idx}].weight out of range`);
    let enabled = true;
    if (typeof r.enabled === "boolean") enabled = r.enabled;
    else if (typeof r.enabled === "string") enabled = r.enabled.toLowerCase() !== "false";
    return { assetSymbol, weight: w, enabled };
  });
}

function portfolioDetail(portfolioId: string): BackendResult {
  if (!isUuid(portfolioId)) return err(400, { error: "portfolioId_invalid" });
  const s = state();
  if (portfolioId !== s.portfolio.portfolioId) return err(404, { error: "portfolio_not_found" });

  const targets = s.targets.map((t) => ({
    assetSymbol: t.assetSymbol,
    targetWeight: dstr(t.targetWeight),
    minWeight: t.minWeight === null ? null : dstr(t.minWeight),
    maxWeight: t.maxWeight === null ? null : dstr(t.maxWeight),
    enabled: t.enabled,
  }));

  const positions = s.positions.map((p) => ({
    assetSymbol: p.assetSymbol,
    qty: dstr(p.qty),
    value: dstr(p.marketValue),
    price: dstr(p.price),
  }));

  const base = {
    portfolioId,
    name: s.portfolio.name,
    baseCurrency: s.portfolio.baseCurrency,
    targets,
    positions,
    quoteCurrency: s.portfolio.baseCurrency,
    drift: [] as AnalyzeResult["drift"],
    currentWeights: [] as Array<{ assetSymbol: string; weight: string }>,
    normalizedTargets: [] as Array<{ assetSymbol: string; weight: string }>,
    nav: null as string | null,
    driftBps: 0,
  };

  if (s.positions.length === 0 || targetsFromPortfolio().length === 0) {
    return ok(base);
  }

  const result = analyzeRebalance({
    portfolioId,
    quoteCurrency: s.portfolio.baseCurrency,
    thresholdBps: 0,
    targets: targetsFromPortfolio(),
    positions: s.positions,
    slipBpsBuy: 0,
    slipBpsSell: 0,
  });

  base.quoteCurrency = result.quoteCurrency;
  base.nav = result.nav;
  base.driftBps = maxAbsDriftBps(result.drift);
  base.drift = result.drift;
  base.currentWeights = result.current.map((x) => ({ assetSymbol: x.assetSymbol, weight: x.weight }));
  base.normalizedTargets = result.normalizedTargets.map((x) => ({ assetSymbol: x.assetSymbol, weight: x.weight }));

  return ok(base);
}

function putTargets(portfolioId: string, body: Record<string, unknown>): BackendResult {
  if (!isUuid(portfolioId)) return err(400, { error: "portfolioId_invalid" });
  if (!Array.isArray(body.targets)) {
    return err(400, { error: "targets_invalid", message: "expected { targets: [...] }" });
  }
  const s = state();
  if (portfolioId !== s.portfolio.portfolioId) return err(404, { error: "portfolio_not_found" });

  const parsed: DemoTarget[] = [];
  for (const raw of body.targets) {
    if (!raw || typeof raw !== "object" || Array.isArray(raw)) return err(400, { error: "target_item_invalid" });
    const r = raw as Record<string, unknown>;
    const assetSymbol = typeof r.assetSymbol === "string" ? r.assetSymbol.trim() : "";
    if (assetSymbol === "") return err(400, { error: "assetSymbol_required" });
    const wRaw = r.weight ?? r.targetWeight;
    const weight = typeof wRaw === "number" ? wRaw : Number(wRaw);
    if (!Number.isFinite(weight) || weight < 0 || weight > 1) {
      return err(400, { error: "weight_out_of_range", message: `weight for ${assetSymbol} must be a number in [0, 1]` });
    }
    const minW = r.minWeight === null || r.minWeight === undefined ? null : Number(r.minWeight);
    const maxW = r.maxWeight === null || r.maxWeight === undefined ? null : Number(r.maxWeight);
    const enabled = typeof r.enabled === "boolean" ? r.enabled : true;
    parsed.push({ assetSymbol, targetWeight: weight, minWeight: minW, maxWeight: maxW, enabled });
  }

  const sumEnabled = parsed.reduce((a, t) => a + (t.enabled ? t.targetWeight : 0), 0);
  if (Math.abs(sumEnabled - 1) > 0.001) {
    return err(400, {
      error: "sum_must_equal_one",
      message: `sum of enabled weights must be ≈ 1.0 (got ${sumEnabled.toFixed(6)}, tolerance ±0.001)`,
    });
  }

  const seen = new Set<string>();
  for (const t of parsed) {
    if (seen.has(t.assetSymbol)) {
      return err(400, { error: "duplicate_asset", message: `asset ${t.assetSymbol} appears more than once` });
    }
    seen.add(t.assetSymbol);
  }

  const before = s.targets.map((t) => ({ assetSymbol: t.assetSymbol, targetWeight: dstr(t.targetWeight), enabled: t.enabled }));
  s.targets = parsed;

  appendAudit({
    actorId: "anonymous",
    action: "portfolio.targets.updated",
    entityType: "portfolio",
    entityId: portfolioId,
    payload: { before, after: parsed.map((t) => ({ assetSymbol: t.assetSymbol, targetWeight: dstr(t.targetWeight), enabled: t.enabled })) },
  });

  return ok({
    portfolioId,
    targets: parsed.map((t) => ({
      assetSymbol: t.assetSymbol,
      targetWeight: dstr(t.targetWeight),
      minWeight: t.minWeight === null ? null : dstr(t.minWeight),
      maxWeight: t.maxWeight === null ? null : dstr(t.maxWeight),
      enabled: t.enabled,
    })),
  });
}

function imitateDrift(portfolioId: string, body: Record<string, unknown>): BackendResult {
  if (!isUuid(portfolioId)) return err(400, { error: "portfolioId_invalid" });
  const s = state();
  if (portfolioId !== s.portfolio.portfolioId) return err(404, { error: "portfolio_not_found" });

  const assetSymbol = typeof body.assetSymbol === "string" ? body.assetSymbol.trim() : "";
  const driftWeight = typeof body.driftWeight === "number" ? body.driftWeight : Number(body.driftWeight);
  if (assetSymbol === "") return err(400, { error: "assetSymbol_required" });
  if (!Number.isFinite(driftWeight)) {
    return err(400, { error: "driftWeight_required", message: "driftWeight (decimal share, e.g. 0.15) is required" });
  }

  const activeTargets = s.targets.filter((t) => t.enabled);
  if (activeTargets.length < 2) {
    return err(400, {
      error: "at_least_two_assets_required",
      message: `Drift imitator needs at least 2 enabled targets (got ${activeTargets.length}).`,
    });
  }

  const tgt = activeTargets.find((t) => t.assetSymbol === assetSymbol);
  if (!tgt) return err(400, { error: "asset_not_in_targets", message: `${assetSymbol} is not an enabled target` });
  const others = activeTargets.filter((t) => t.assetSymbol !== assetSymbol);
  if (others.length === 0) return err(500, { error: "internal_error", message: "could not find counter-assets" });
  const othersTargetSum = others.reduce((a, t) => a + t.targetWeight, 0);
  if (!(othersTargetSum > 0)) {
    return err(400, { error: "counter_assets_zero_target", message: "Cannot redistribute drift — all counter-assets have target weight 0." });
  }

  const snaps = s.positions;
  const firstSnap = snaps[0];
  if (!firstSnap) return err(400, { error: "no_position_snapshots" });
  const snapByAsset = new Map(snaps.map((p) => [p.assetSymbol, p] as const));
  const navTotal = snaps.reduce((a, p) => a + p.marketValue, 0);
  if (!Number.isFinite(navTotal) || navTotal <= 0) return err(400, { error: "nav_not_positive" });

  const priceFor = (sym: string) => snapByAsset.get(sym)?.price ?? firstSnap.price;

  const targetW = tgt.targetWeight;
  const newActualW = targetW + driftWeight;
  if (newActualW < 0 || newActualW > 1) {
    return err(400, {
      error: "drift_out_of_range",
      message: `target ${dstr(targetW)} + drift ${dstr(driftWeight)} = ${dstr(newActualW)}, must be in [0, 1]`,
    });
  }
  const remainder = 1 - newActualW;

  const priceTgt = priceFor(tgt.assetSymbol);
  if (!(priceTgt > 0)) return err(400, { error: "price_not_positive", message: `price for ${tgt.assetSymbol} is not positive` });

  const newSnapshots: DemoPosition[] = [];
  const newWeights: Record<string, number> = { [tgt.assetSymbol]: newActualW };

  const newMvTgt = newActualW * navTotal;
  newSnapshots.push({ assetSymbol: tgt.assetSymbol, qty: newMvTgt / priceTgt, marketValue: newMvTgt, price: priceTgt });

  for (const other of others) {
    const otherActualW = remainder * (other.targetWeight / othersTargetSum);
    const priceOther = priceFor(other.assetSymbol);
    if (!(priceOther > 0)) return err(400, { error: "price_not_positive", message: `price for ${other.assetSymbol} is not positive` });
    const mvOther = otherActualW * navTotal;
    newSnapshots.push({ assetSymbol: other.assetSymbol, qty: mvOther / priceOther, marketValue: mvOther, price: priceOther });
    newWeights[other.assetSymbol] = otherActualW;
  }

  s.positions = newSnapshots;

  appendAudit({
    actorId: "anonymous",
    action: "portfolio.drift.imitated",
    entityType: "portfolio",
    entityId: portfolioId,
    payload: { assetSymbol, driftWeight, navTotal, newWeights },
  });

  return ok({
    portfolioId,
    nav: dstr(navTotal),
    applied: {
      assetSymbol,
      driftWeight,
      newActualWeights: Object.entries(newWeights).map(([sym, w]) => ({ assetSymbol: sym, weight: dstr(w) })),
    },
  });
}

function rebalanceNow(portfolioId: string): BackendResult {
  if (!isUuid(portfolioId)) return err(400, { error: "portfolioId_invalid" });
  const s = state();
  if (portfolioId !== s.portfolio.portfolioId) return err(404, { error: "portfolio_not_found" });
  if (s.positions.length === 0) return err(400, { error: "no_position_snapshots" });
  const targets = targetsFromPortfolio();
  if (targets.length === 0) return err(400, { error: "targets_empty" });

  let result: AnalyzeResult;
  try {
    result = analyzeRebalance({
      portfolioId,
      quoteCurrency: s.portfolio.baseCurrency,
      thresholdBps: 0,
      targets,
      positions: s.positions,
      slipBpsBuy: 0,
      slipBpsSell: 0,
    });
  } catch (e) {
    return err(400, { error: "rebalance_failed", message: e instanceof Error ? e.message : "unknown_error" });
  }

  const navTotal = Number(result.nav);
  const quote = s.portfolio.baseCurrency;
  const priceByAsset = new Map(s.positions.map((p) => [p.assetSymbol, p.price] as const));

  const transfers = result.plannedOrders
    .map((o) => {
      const qty = Number(o.qty);
      const price = Number(o.price);
      const notional = qty * price;
      if (!Number.isFinite(notional) || notional <= 0) return null;
      if (o.side === "buy") {
        return {
          from: quote,
          to: o.assetSymbol,
          amountFrom: notional.toFixed(6),
          amountTo: qty.toFixed(6),
          txHash: mockTxHash(),
          note: `Convert ${notional.toFixed(2)} ${quote} into ${qty.toFixed(4)} ${o.assetSymbol} at ${dstr(price)}`,
        };
      }
      return {
        from: o.assetSymbol,
        to: quote,
        amountFrom: qty.toFixed(6),
        amountTo: notional.toFixed(6),
        txHash: mockTxHash(),
        note: `Convert ${qty.toFixed(4)} ${o.assetSymbol} into ${notional.toFixed(2)} ${quote} at ${dstr(price)}`,
      };
    })
    .filter((t): t is NonNullable<typeof t> => t !== null);

  // Rewrite snapshots to target weights (closes the loop visually).
  const newSnaps: DemoPosition[] = [];
  const known = new Set<string>();
  for (const t of result.normalizedTargets) {
    const w = Number(t.weight);
    const mv = w * navTotal;
    const price = priceByAsset.get(t.assetSymbol);
    if (price === undefined || !Number.isFinite(price) || price <= 0) continue;
    newSnaps.push({ assetSymbol: t.assetSymbol, qty: mv / price, marketValue: mv, price });
    known.add(t.assetSymbol);
  }
  for (const p of s.positions) {
    if (!known.has(p.assetSymbol)) newSnaps.push({ ...p });
  }
  s.positions = newSnaps;

  appendAudit({
    actorId: "anonymous",
    action: "portfolio.rebalance.now",
    entityType: "portfolio",
    entityId: portfolioId,
    payload: { mode: "plan_only", transfersCount: transfers.length, nav: result.nav },
  });

  return ok({
    portfolioId,
    mode: "plan_only",
    isDemo: true,
    nav: result.nav,
    transfers,
    plannedOrdersCount: result.plannedOrders.length,
  });
}

function rebalancePreview(body: Record<string, unknown>): BackendResult {
  try {
    const portfolioId = typeof body.portfolioId === "string" ? body.portfolioId : "";
    if (!isUuid(portfolioId)) return err(400, { error: "portfolioId_invalid" });
    const s = state();
    if (portfolioId !== s.portfolio.portfolioId) return err(404, { error: "portfolio_not_found" });
    if (s.positions.length === 0) return err(400, { error: "no_position_snapshots" });

    const thresholdBps =
      typeof body.thresholdBps === "undefined"
        ? DEFAULT_THRESHOLD_BPS
        : (() => {
            const n = typeof body.thresholdBps === "number" ? body.thresholdBps : Number(body.thresholdBps);
            if (!Number.isFinite(n) || n < 0) throw new Error("thresholdBps must be a finite non-negative number");
            return n;
          })();

    const decoded = decodeTargets(body.targets);
    const targets = decoded && decoded.length > 0 ? decoded : targetsFromPortfolio();
    if (targets.length === 0) return err(400, { error: "targets_empty" });

    const result = analyzeRebalance({
      portfolioId,
      quoteCurrency: s.portfolio.baseCurrency,
      thresholdBps,
      targets,
      positions: s.positions,
      slipBpsBuy: SLIP_BPS,
      slipBpsSell: SLIP_BPS,
    });

    const previewId = crypto.randomUUID();
    appendAudit({
      actorId: "anonymous",
      action: "rebalance.preview",
      entityType: "rebalance_job",
      entityId: previewId,
      payload: { portfolioId },
    });

    return ok({
      previewId,
      current: result.current.map((x) => ({ assetSymbol: x.assetSymbol, weight: x.weight })),
      drift: result.drift.map((d) => ({
        assetSymbol: d.assetSymbol,
        targetWeight: d.targetWeight,
        currentWeight: d.currentWeight,
        driftWeightBps: d.driftWeightBps,
        skippedDueToThreshold: d.skippedDueToThreshold,
      })),
      normalizedTargets: result.normalizedTargets.map((x) => ({ assetSymbol: x.assetSymbol, weight: x.weight })),
      nav: result.nav,
      warnings: result.warnings,
      plannedOrders: result.plannedOrders.map((o) => ({
        market: o.market,
        assetSymbol: o.assetSymbol,
        side: o.side,
        qty: o.qty,
        type: o.type,
        price: o.price,
      })),
      estimatedNotional: result.estimatedNotional,
      riskForLive: { liveExecuteAllowed: true, violations: [] as Array<{ code: string; message: string }> },
    });
  } catch (e) {
    return err(400, { error: "preview_failed", message: e instanceof Error ? e.message : "unknown_error" });
  }
}

function rebalanceExecute(body: Record<string, unknown>): BackendResult {
  const portfolioId = typeof body.portfolioId === "string" ? body.portfolioId : "";
  try {
    if (!isUuid(portfolioId)) return err(400, { error: "portfolioId_invalid" });
    const s = state();
    if (portfolioId !== s.portfolio.portfolioId) return err(404, { error: "portfolio_not_found" });
    if (s.positions.length === 0) return err(400, { error: "no_position_snapshots" });

    const thresholdBps =
      typeof body.thresholdBps === "undefined"
        ? DEFAULT_THRESHOLD_BPS
        : (() => {
            const n = typeof body.thresholdBps === "number" ? body.thresholdBps : Number(body.thresholdBps);
            if (!Number.isFinite(n) || n < 0) throw new Error("thresholdBps must be a finite non-negative number");
            return n;
          })();

    const decoded = decodeTargets(body.targets);
    const targets = decoded && decoded.length > 0 ? decoded : targetsFromPortfolio();
    if (targets.length === 0) return err(400, { error: "targets_empty" });

    const dryRunRaw = body.dryRun;
    const dryRun = typeof dryRunRaw === "boolean" ? dryRunRaw : String(dryRunRaw).toLowerCase() === "true";

    const analyzed = analyzeRebalance({
      portfolioId,
      quoteCurrency: s.portfolio.baseCurrency,
      thresholdBps,
      targets,
      positions: s.positions,
      slipBpsBuy: SLIP_BPS,
      slipBpsSell: SLIP_BPS,
    });

    const jobId = crypto.randomUUID();
    const input = {
      kind: "execute",
      portfolioId,
      thresholdBps,
      targets: targets.map((t) => ({ assetSymbol: t.assetSymbol, weight: t.weight, enabled: t.enabled ?? true })),
      dryRun,
      positions: s.positions.map((p) => ({ assetSymbol: p.assetSymbol, qty: dstr(p.qty), price: dstr(p.price), marketValue: dstr(p.marketValue) })),
    };

    s.jobs.set(jobId, {
      jobId,
      portfolioId,
      mode: dryRun ? "dry_run_execute" : "live_execute",
      dryRun,
      requestedBy: "anonymous",
      createdAtMs: Date.now(),
      input,
      plannedOrders: analyzed.plannedOrders,
      nav: analyzed.nav,
      estimatedNotional: analyzed.estimatedNotional,
    });

    appendAudit({
      actorId: "anonymous",
      action: "rebalance.execute.enqueued",
      entityType: "rebalance_job",
      entityId: jobId,
      payload: { portfolioId, dryRun },
    });

    return ok({ jobId, status: "queued" });
  } catch (e) {
    return err(400, { error: "execute_failed", message: e instanceof Error ? e.message : "unknown_error" });
  }
}

// Job progression by elapsed time (no timers): queued → running → completed.
const QUEUED_MS = 3000;
const RUNNING_MS = 8000;

function jobPhase(job: DemoJob): "queued" | "running" | "completed" {
  const elapsed = Date.now() - job.createdAtMs;
  if (elapsed < QUEUED_MS) return "queued";
  if (elapsed < RUNNING_MS) return "running";
  return "completed";
}

function buildBatches(job: DemoJob, phase: "queued" | "running" | "completed") {
  if (phase === "queued" || job.plannedOrders.length === 0) return [];

  const done = phase === "completed";
  const createdIso = new Date(job.createdAtMs).toISOString();
  const updatedIso = new Date(job.createdAtMs + (done ? RUNNING_MS : QUEUED_MS)).toISOString();

  // Group planned orders by market (one batch per market/venue).
  const byMarket = new Map<string, PlannedOrder[]>();
  for (const o of job.plannedOrders) {
    const list = byMarket.get(o.market) ?? [];
    list.push(o);
    byMarket.set(o.market, list);
  }

  return [...byMarket.entries()].map(([market, orders]) => {
    const orderRows = orders.map((o) => ({
      orderId: crypto.randomUUID(),
      venue: "silvana",
      venueOrderRef: done ? `sv-${randomHex(6)}` : `sv-${randomHex(6)}`,
      execProfile: "book",
      silvanaOrderId: done ? `silvana-${randomHex(8)}` : null,
      side: o.side,
      type: o.type,
      market: o.market,
      price: o.price,
      qty: o.qty,
      status: done ? "filled" : "submitted",
      clientOrderRef: `rebal-${job.jobId.slice(0, 8)}`,
      createdAt: createdIso,
      updatedAt: updatedIso,
    }));

    return {
      batchId: crypto.randomUUID(),
      status: done ? "completed" : "submitting",
      venue: "silvana",
      market,
      totalOrders: orderRows.length,
      submittedOrders: orderRows.length,
      filledOrders: done ? orderRows.length : 0,
      cancelledOrders: 0,
      orders: orderRows,
    };
  });
}

function jobDetail(jobId: string): BackendResult {
  if (!isUuid(jobId)) return err(400, { error: "jobId_invalid" });
  const job = state().jobs.get(jobId);
  if (!job) return err(404, { error: "job_not_found" });

  const phase = jobPhase(job);
  const status = phase; // "queued" | "running" | "completed"
  const startedAt = phase === "queued" ? null : new Date(job.createdAtMs + QUEUED_MS).toISOString();
  const finishedAt = phase === "completed" ? new Date(job.createdAtMs + RUNNING_MS).toISOString() : null;
  const batches = buildBatches(job, phase);

  let output: unknown = null;
  if (phase === "running") {
    output = { phase: "submitting", nav: job.nav, plannedOrdersCount: job.plannedOrders.length };
  } else if (phase === "completed") {
    output = {
      phase: "completed",
      nav: job.nav,
      estimatedNotional: job.estimatedNotional,
      plannedOrdersCount: job.plannedOrders.length,
      dryRun: job.dryRun,
    };
  }

  const settlements =
    phase === "completed" && job.plannedOrders.length > 0
      ? [
          {
            id: crypto.randomUUID(),
            status: "settled",
            payload: {
              phase: "settled",
              nav: job.nav,
              orders: job.plannedOrders.length,
              dryRun: job.dryRun,
            },
            createdAt: finishedAt,
          },
        ]
      : [];

  return ok({
    jobId: job.jobId,
    portfolioId: job.portfolioId,
    status,
    mode: job.mode,
    dryRun: job.dryRun,
    requestedBy: job.requestedBy,
    input: job.input,
    output,
    startedAt,
    finishedAt,
    createdAt: new Date(job.createdAtMs).toISOString(),
    batches,
    settlements,
  });
}

function jobSummary(jobId: string): BackendResult {
  if (!isUuid(jobId)) return err(400, { error: "jobId_invalid" });
  const job = state().jobs.get(jobId);
  if (!job) return err(404, { error: "job_not_found" });
  const phase = jobPhase(job);
  const batches = buildBatches(job, phase).map((b) => ({
    batchId: b.batchId,
    status: b.status,
    venue: b.venue,
    submittedOrders: b.submittedOrders,
    filledOrders: b.filledOrders,
  }));
  return ok({ jobId: job.jobId, status: phase, batches });
}

// Venues (mirror shapes of apps/api venues + execution routes; realistic demo config).
function venuesStatus(): BackendResult {
  return ok({
    checkedAt: new Date().toISOString(),
    executionRouterEnabled: true,
    venues: [
      { venue: "silvana", label: "Silvana Orderbook", configured: true, mode: "rpc_ready", notes: [] },
      { venue: "okx", label: "OKX Spot (REST v5)", configured: true, mode: "simulated_live", notes: [] },
      { venue: "temple", label: "Temple / Canton", configured: true, mode: "stub", notes: [] },
      {
        venue: "okx-liquid",
        label: "OKX Liquidity (RFQ)",
        configured: false,
        mode: "not_configured",
        notes: ["Set OKX_LIQUID_STUB_ACCEPT or OKX_LIQUID_API_BASE_URL once RFQ REST is wired"],
      },
      {
        venue: "binance-otc",
        label: "Binance OTC",
        configured: false,
        mode: "not_configured",
        notes: ["OTC API and keys are allowed only after compliance review"],
      },
    ],
  });
}

const KNOWN_VENUES = new Set(["silvana", "okx", "temple", "okx-liquid", "binance-otc"]);

function chooseVenue(intent: {
  venue?: string;
  market: string;
  side: string;
  type: string;
  qty: string;
  execProfile?: string;
}): string {
  if (intent.venue) return intent.venue;
  if (intent.execProfile === "rfq" || intent.execProfile === "block") return "okx-liquid";
  if (intent.execProfile === "otc") return "binance-otc";
  if (intent.type === "market") return "okx";
  const qtyNorm = intent.qty.trim();
  if (/^[0-9]+(\.[0-9]+)?$/.test(qtyNorm) && Number(qtyNorm) >= 0) return "okx-liquid";
  if (/canton/i.test(intent.market)) return "temple";
  return "silvana";
}

function previewRoute(body: Record<string, unknown>): BackendResult {
  const market = typeof body.market === "string" ? body.market.trim() : "";
  const side = body.side === "sell" ? "sell" : body.side === "buy" ? "buy" : undefined;
  const type = body.type === "market" ? "market" : body.type === "limit" ? "limit" : undefined;
  const qty = typeof body.qty === "string" ? body.qty.trim() : "";

  if (!market.length || !side || !type || !qty.length) {
    return err(400, { error: "market, side (buy|sell), type (limit|market), qty are required" });
  }

  const requestedVenue = typeof body.venue === "string" ? body.venue.trim() : "";
  if (requestedVenue.length > 0 && !KNOWN_VENUES.has(requestedVenue)) {
    return err(400, { error: "unknown venue" });
  }

  const ep = typeof body.execProfile === "string" ? body.execProfile.trim() : "";
  if (ep.length > 0 && !["book", "rfq", "block", "otc"].includes(ep)) {
    return err(400, { error: "execProfile must be book|rfq|block|otc when set" });
  }

  const priceRaw = typeof body.price === "string" ? body.price.trim() : undefined;
  const intent: Record<string, unknown> = { market, side, type, qty };
  if (priceRaw && priceRaw.length) intent.price = priceRaw;
  if (ep.length) intent.execProfile = ep;
  if (requestedVenue.length) intent.venue = requestedVenue;
  if (typeof body.clientOrderRef === "string" && body.clientOrderRef.trim().length > 0) {
    intent.clientOrderRef = body.clientOrderRef.trim();
  }

  const chosenVenue = chooseVenue({
    venue: requestedVenue.length ? requestedVenue : undefined,
    market,
    side,
    type,
    qty,
    execProfile: ep.length ? ep : undefined,
  });

  return ok({
    checkedAt: new Date().toISOString(),
    intent,
    routerConfig: { defaultVenue: "silvana", minQtyRfQ: "0", maxQtyOkx: "50000", maxQtyLiquid: null },
    chosenVenue,
    risk: { ok: true },
  });
}

function auditEntries(limit: number): BackendResult {
  const clamped = Number.isFinite(limit) ? Math.min(Math.max(limit, 1), 500) : 100;
  const logs = [...state().audit].reverse().slice(0, clamped);
  return ok({ logs });
}

// ---------------------------------------------------------------------------
// Dispatcher
// ---------------------------------------------------------------------------

/**
 * Resolve a would-be `apps/api` request in-process against demo fixtures.
 * `pathname` may include a query string (e.g. `/api/audit/entries?limit=200`).
 */
export function handleBackend(method: string, pathname: string, bodyText?: string): BackendResult {
  const m = method.toUpperCase();
  const [rawPath, rawQuery = ""] = pathname.split("?");
  const path = (rawPath ?? "").replace(/\/+$/, "") || "/";
  const query = new URLSearchParams(rawQuery);
  const body = parseBody(bodyText);

  // Health probe
  if (path === "/" && m === "GET") {
    return ok({ ok: true, service: "batch-order-agent-api", mode: "demo" });
  }

  // Portfolio collection
  if (path === "/api/portfolio" && m === "GET") {
    const s = state();
    return ok({ portfolios: [{ id: s.portfolio.portfolioId, name: s.portfolio.name, baseCurrency: s.portfolio.baseCurrency }] });
  }

  // Portfolio sub-routes: /api/portfolio/:id[/targets|/rebalance-now|/imitate-drift]
  const pf = path.match(/^\/api\/portfolio\/([^/]+)(?:\/(targets|rebalance-now|imitate-drift))?$/);
  if (pf) {
    const id = decodeURIComponent(pf[1] ?? "");
    const sub = pf[2];
    if (!sub && m === "GET") return portfolioDetail(id);
    if (sub === "targets" && m === "PUT") return putTargets(id, body);
    if (sub === "rebalance-now" && m === "POST") return rebalanceNow(id);
    if (sub === "imitate-drift" && m === "POST") return imitateDrift(id, body);
    return err(405, { error: "method_not_allowed" });
  }

  // Rebalance
  if (path === "/api/rebalance/preview" && m === "POST") return rebalancePreview(body);
  if (path === "/api/rebalance/execute" && m === "POST") return rebalanceExecute(body);
  const job = path.match(/^\/api\/rebalance\/jobs\/([^/]+)$/);
  if (job && m === "GET") {
    const id = decodeURIComponent(job[1] ?? "");
    const detail = query.get("detail");
    const detailed = detail === "1" || detail === "true" || detail === "yes";
    return detailed ? jobDetail(id) : jobSummary(id);
  }

  // Venues + execution
  if (path === "/api/venues/status" && m === "GET") return venuesStatus();
  if (path === "/api/execution/preview-route" && m === "POST") return previewRoute(body);

  // Audit
  if (path === "/api/audit/entries" && m === "GET") {
    const raw = Number(query.get("limit") ?? 100);
    return auditEntries(raw);
  }

  return err(404, { error: "not_found", message: `no demo fixture for ${m} ${path}` });
}
