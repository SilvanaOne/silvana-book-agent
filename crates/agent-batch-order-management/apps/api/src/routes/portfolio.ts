import { randomBytes } from "node:crypto";
import { Router } from "express";
import {
  appendAuditLog,
  findPortfolioById,
  listLatestSnapshotsForPortfolio,
  listPortfolioSummaries,
  replaceLatestPositionSnapshots,
  replacePortfolioTargets,
} from "@batch-order/db";
import { analyzeRebalance } from "@batch-order/portfolio-engine";
import { isUuid } from "../lib/ids.js";
import { actorId } from "../lib/request-context.js";
import { snapshotsToPositions, targetsFromPortfolio } from "../lib/rebalance-input.js";

export const portfolioRouter = Router();

const WEIGHT_SUM_TOLERANCE = 0.001;

function isPlainObject(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

function parseFiniteNumber(v: unknown): number | null {
  if (typeof v === "number") return Number.isFinite(v) ? v : null;
  if (typeof v === "string" && v.trim() !== "") {
    const n = Number(v);
    return Number.isFinite(n) ? n : null;
  }
  return null;
}

function maxAbsDriftBps(drift: ReadonlyArray<{ driftWeightBps: string }>): number {
  let m = 0;
  for (const d of drift) {
    const v = Number(d.driftWeightBps);

    if (Number.isFinite(v)) {
      const absRounded = Math.abs(Math.round(v));

      if (absRounded > m) m = absRounded;
    }
  }
  return m;
}

function mapTargets(portfolioTargets: NonNullable<Awaited<ReturnType<typeof findPortfolioById>>>["targets"]) {
  return portfolioTargets.map((t) => ({
    assetSymbol: t.assetSymbol,
    targetWeight: t.targetWeight.toString(),
    minWeight: t.minWeight?.toString() ?? null,
    maxWeight: t.maxWeight?.toString() ?? null,
    enabled: t.enabled,
  }));
}

/** Collection — must be registered before `/:id` so `/` is not captured as an id. */
portfolioRouter.get("/", async (_req, res) => {
  const portfolios = await listPortfolioSummaries();
  res.json({ portfolios });
});

portfolioRouter.get("/:id", async (req, res) => {
  const portfolioId = req.params.id ?? "";
  if (!isUuid(portfolioId)) {
    res.status(400).json({ error: "portfolioId_invalid" });
    return;
  }

  const portfolio = await findPortfolioById(portfolioId);
  if (!portfolio) {
    res.status(404).json({ error: "portfolio_not_found" });
    return;
  }

  const snaps = await listLatestSnapshotsForPortfolio(portfolioId);
  const targets = targetsFromPortfolio(portfolio.targets);

  const base = {
    portfolioId,
    name: portfolio.name,
    baseCurrency: portfolio.baseCurrency,
    targets: mapTargets(portfolio.targets),
    positions: snaps.map((s) => ({
      assetSymbol: s.assetSymbol,

      qty: s.qty.toString(),
      value: s.marketValue.toString(),
      price: s.price.toString(),
    })),

    quoteCurrency: portfolio.baseCurrency,

    drift: [] as Array<{
      assetSymbol: string;
      targetWeight: string;
      currentWeight: string;
      driftWeightBps: string;
      skippedDueToThreshold: boolean;
    }>,
    currentWeights: [] as Array<{ assetSymbol: string; weight: string }>,
    normalizedTargets: [] as Array<{ assetSymbol: string; weight: string }>,
    nav: null as string | null,
    driftBps: 0,
  };

  if (snaps.length === 0 || targets.length === 0) {
    res.json(base);
    return;
  }

  const result = analyzeRebalance({
    portfolioId,
    quoteCurrency: portfolio.baseCurrency,
    thresholdBps: 0,
    targets,
    positions: snapshotsToPositions(snaps),
    slipBpsBuy: 0,
    slipBpsSell: 0,
  });

  base.quoteCurrency = result.quoteCurrency;
  base.nav = result.nav;

  base.driftBps = maxAbsDriftBps(result.drift);
  base.drift = result.drift.map((d) => ({
    assetSymbol: d.assetSymbol,
    targetWeight: d.targetWeight,
    currentWeight: d.currentWeight,
    driftWeightBps: d.driftWeightBps,
    skippedDueToThreshold: d.skippedDueToThreshold,
  }));

  base.currentWeights = result.current.map((x) => ({ assetSymbol: x.assetSymbol, weight: x.weight }));
  base.normalizedTargets = result.normalizedTargets.map((x) => ({ assetSymbol: x.assetSymbol, weight: x.weight }));

  res.json(base);
});

portfolioRouter.put("/:id/targets", async (req, res) => {
  const portfolioId = req.params.id ?? "";
  if (!isUuid(portfolioId)) {
    res.status(400).json({ error: "portfolioId_invalid" });
    return;
  }

  const body = req.body as Record<string, unknown>;
  if (!Array.isArray(body.targets)) {
    res.status(400).json({ error: "targets_invalid", message: "expected { targets: [...] }" });
    return;
  }

  const parsed: Array<{ assetSymbol: string; targetWeight: string; minWeight: string | null; maxWeight: string | null; enabled: boolean }> = [];
  for (const raw of body.targets) {
    if (!isPlainObject(raw)) {
      res.status(400).json({ error: "target_item_invalid" });
      return;
    }
    const assetSymbol = typeof raw.assetSymbol === "string" ? raw.assetSymbol.trim() : "";
    if (assetSymbol === "") {
      res.status(400).json({ error: "assetSymbol_required" });
      return;
    }
    const weight = parseFiniteNumber(raw.weight ?? raw.targetWeight);
    if (weight === null || weight < 0 || weight > 1) {
      res.status(400).json({ error: "weight_out_of_range", message: `weight for ${assetSymbol} must be a number in [0, 1]` });
      return;
    }
    const minW = raw.minWeight === null || raw.minWeight === undefined ? null : parseFiniteNumber(raw.minWeight);
    const maxW = raw.maxWeight === null || raw.maxWeight === undefined ? null : parseFiniteNumber(raw.maxWeight);
    if ((raw.minWeight !== null && raw.minWeight !== undefined && minW === null) ||
        (raw.maxWeight !== null && raw.maxWeight !== undefined && maxW === null)) {
      res.status(400).json({ error: "min_or_max_invalid" });
      return;
    }
    const enabled = typeof raw.enabled === "boolean" ? raw.enabled : true;
    parsed.push({
      assetSymbol,
      targetWeight: String(weight),
      minWeight: minW === null ? null : String(minW),
      maxWeight: maxW === null ? null : String(maxW),
      enabled,
    });
  }

  const sumEnabled = parsed.reduce((acc, t) => acc + (t.enabled ? Number(t.targetWeight) : 0), 0);
  if (Math.abs(sumEnabled - 1) > WEIGHT_SUM_TOLERANCE) {
    res.status(400).json({
      error: "sum_must_equal_one",
      message: `sum of enabled weights must be ≈ 1.0 (got ${sumEnabled.toFixed(6)}, tolerance ±${WEIGHT_SUM_TOLERANCE})`,
    });
    return;
  }

  const dupes = new Set<string>();
  for (const t of parsed) {
    if (dupes.has(t.assetSymbol)) {
      res.status(400).json({ error: "duplicate_asset", message: `asset ${t.assetSymbol} appears more than once` });
      return;
    }
    dupes.add(t.assetSymbol);
  }

  const portfolio = await findPortfolioById(portfolioId);
  if (!portfolio) {
    res.status(404).json({ error: "portfolio_not_found" });
    return;
  }

  const before = portfolio.targets.map((t) => ({
    assetSymbol: t.assetSymbol,
    targetWeight: t.targetWeight.toString(),
    enabled: t.enabled,
  }));

  const saved = await replacePortfolioTargets(portfolioId, parsed);

  await appendAuditLog({
    actorId: actorId(req),
    action: "portfolio.targets.updated",
    entityType: "portfolio",
    entityId: portfolioId,
    payload: { before, after: parsed },
  });

  res.json({
    portfolioId,
    targets: saved.map((t) => ({
      assetSymbol: t.assetSymbol,
      targetWeight: t.targetWeight.toString(),
      minWeight: t.minWeight?.toString() ?? null,
      maxWeight: t.maxWeight?.toString() ?? null,
      enabled: t.enabled,
    })),
  });
});

function isDemoToolsEnabled(): boolean {
  const v = (process.env.DEMO_TOOLS ?? "").trim().toLowerCase();
  return v === "1" || v === "true" || v === "yes";
}

portfolioRouter.post("/:id/imitate-drift", async (req, res) => {
  if (!isDemoToolsEnabled()) {
    res.status(404).json({ error: "not_found" });
    return;
  }

  const portfolioId = req.params.id ?? "";
  if (!isUuid(portfolioId)) {
    res.status(400).json({ error: "portfolioId_invalid" });
    return;
  }

  const body = req.body as Record<string, unknown>;
  const assetSymbol = typeof body.assetSymbol === "string" ? body.assetSymbol.trim() : "";
  const driftWeight = parseFiniteNumber(body.driftWeight);
  if (assetSymbol === "") {
    res.status(400).json({ error: "assetSymbol_required" });
    return;
  }
  if (driftWeight === null) {
    res.status(400).json({ error: "driftWeight_required", message: "driftWeight (decimal share, e.g. 0.15) is required" });
    return;
  }

  const portfolio = await findPortfolioById(portfolioId);
  if (!portfolio) {
    res.status(404).json({ error: "portfolio_not_found" });
    return;
  }

  const activeTargets = portfolio.targets.filter((t) => t.enabled);
  if (activeTargets.length !== 2) {
    res.status(400).json({
      error: "two_assets_required",
      message: `Drift imitator currently supports exactly 2 enabled targets (got ${activeTargets.length}).`,
    });
    return;
  }

  const tgt = activeTargets.find((t) => t.assetSymbol === assetSymbol);
  if (!tgt) {
    res.status(400).json({ error: "asset_not_in_targets", message: `${assetSymbol} is not an enabled target` });
    return;
  }
  const other = activeTargets.find((t) => t.assetSymbol !== assetSymbol);
  if (!other) {
    res.status(500).json({ error: "internal_error", message: "could not find counter-asset" });
    return;
  }

  const snaps = await listLatestSnapshotsForPortfolio(portfolioId);
  const firstSnap = snaps[0];
  if (!firstSnap) {
    res.status(400).json({ error: "no_position_snapshots" });
    return;
  }
  const snapByAsset = new Map(snaps.map((s) => [s.assetSymbol, s] as const));
  const navTotal = snaps.reduce((acc, s) => acc + Number(s.marketValue.toString()), 0);
  if (!Number.isFinite(navTotal) || navTotal <= 0) {
    res.status(400).json({ error: "nav_not_positive" });
    return;
  }

  const targetW = Number(tgt.targetWeight.toString());
  const newActualW = targetW + driftWeight;
  if (newActualW < 0 || newActualW > 1) {
    res.status(400).json({
      error: "drift_out_of_range",
      message: `target ${targetW} + drift ${driftWeight} = ${newActualW}, must be in [0, 1]`,
    });
    return;
  }
  const otherActualW = 1 - newActualW;

  const priceTgt = Number((snapByAsset.get(tgt.assetSymbol)?.price ?? firstSnap.price).toString());
  const priceOther = Number((snapByAsset.get(other.assetSymbol)?.price ?? firstSnap.price).toString());
  if (!Number.isFinite(priceTgt) || priceTgt <= 0 || !Number.isFinite(priceOther) || priceOther <= 0) {
    res.status(400).json({ error: "price_not_positive" });
    return;
  }

  const newMvTgt = newActualW * navTotal;
  const newMvOther = otherActualW * navTotal;
  const newQtyTgt = newMvTgt / priceTgt;
  const newQtyOther = newMvOther / priceOther;

  const newSnapshots = [
    {
      assetSymbol: tgt.assetSymbol,
      qty: newQtyTgt.toString(),
      marketValue: newMvTgt.toString(),
      price: priceTgt.toString(),
    },
    {
      assetSymbol: other.assetSymbol,
      qty: newQtyOther.toString(),
      marketValue: newMvOther.toString(),
      price: priceOther.toString(),
    },
  ];

  await replaceLatestPositionSnapshots(portfolioId, newSnapshots);

  await appendAuditLog({
    actorId: actorId(req),
    action: "portfolio.drift.imitated",
    entityType: "portfolio",
    entityId: portfolioId,
    payload: {
      assetSymbol,
      driftWeight,
      navTotal,
      newWeights: { [tgt.assetSymbol]: newActualW, [other.assetSymbol]: otherActualW },
    },
  });

  res.json({
    portfolioId,
    nav: navTotal.toString(),
    applied: {
      assetSymbol,
      driftWeight,
      newActualWeights: [
        { assetSymbol: tgt.assetSymbol, weight: newActualW.toString() },
        { assetSymbol: other.assetSymbol, weight: otherActualW.toString() },
      ],
    },
  });
});

function mockTxHash(): string {
  return "0x" + randomBytes(32).toString("hex");
}

function workerExecutionMode(): string {
  const m = (process.env.WORKER_SILVANA_EXECUTION ?? "").trim().toLowerCase();
  if (m === "rpc" || m === "plan_only") return m;
  const jwt = (process.env.SILVANA_JWT ?? "").trim();
  return jwt.length > 0 ? "rpc" : "plan_only";
}

portfolioRouter.post("/:id/rebalance-now", async (req, res) => {
  const portfolioId = req.params.id ?? "";
  if (!isUuid(portfolioId)) {
    res.status(400).json({ error: "portfolioId_invalid" });
    return;
  }

  const portfolio = await findPortfolioById(portfolioId);
  if (!portfolio) {
    res.status(404).json({ error: "portfolio_not_found" });
    return;
  }

  const snaps = await listLatestSnapshotsForPortfolio(portfolioId);
  if (snaps.length === 0) {
    res.status(400).json({ error: "no_position_snapshots" });
    return;
  }

  const targets = targetsFromPortfolio(portfolio.targets);
  if (targets.length === 0) {
    res.status(400).json({ error: "targets_empty" });
    return;
  }

  const result = analyzeRebalance({
    portfolioId,
    quoteCurrency: portfolio.baseCurrency,
    thresholdBps: 0,
    targets,
    positions: snapshotsToPositions(snaps),
    slipBpsBuy: 0,
    slipBpsSell: 0,
  });

  // Build transfer-friendly summary: pair each planned buy with the offsetting sell
  // (or treat the implicit quote leg as the counter side when only one explicit order).
  const navTotal = Number(result.nav);
  const priceByAsset = new Map<string, number>();
  for (const s of snaps) {
    priceByAsset.set(s.assetSymbol, Number(s.price.toString()));
  }
  const quote = portfolio.baseCurrency;

  type Transfer = {
    from: string;
    to: string;
    amountFrom: string;
    amountTo: string;
    txHash: string;
    note: string;
  };
  const transfers: Transfer[] = [];

  for (const o of result.plannedOrders) {
    const qty = Number(o.qty);
    const price = Number(o.price);
    const notional = qty * price;
    if (!Number.isFinite(notional) || notional <= 0) continue;
    if (o.side === "buy") {
      transfers.push({
        from: quote,
        to: o.assetSymbol,
        amountFrom: notional.toFixed(6),
        amountTo: qty.toFixed(6),
        txHash: mockTxHash(),
        note: `Convert ${notional.toFixed(2)} ${quote} into ${qty.toFixed(4)} ${o.assetSymbol} at ${price}`,
      });
    } else {
      transfers.push({
        from: o.assetSymbol,
        to: quote,
        amountFrom: qty.toFixed(6),
        amountTo: notional.toFixed(6),
        txHash: mockTxHash(),
        note: `Convert ${qty.toFixed(4)} ${o.assetSymbol} into ${notional.toFixed(2)} ${quote} at ${price}`,
      });
    }
  }

  // Build target-aligned PositionSnapshot: each active target now holds exactly its weight × NAV.
  const newSnaps: Array<{ assetSymbol: string; qty: string; marketValue: string; price: string }> = [];
  for (const t of result.normalizedTargets) {
    const w = Number(t.weight);
    const mv = w * navTotal;
    const price = priceByAsset.get(t.assetSymbol);
    if (price === undefined || !Number.isFinite(price) || price <= 0) {
      // Unknown price (target without a current position) — fall back to keeping notional in quote currency.
      // Skip writing this asset; the quote currency row will absorb everything.
      continue;
    }
    newSnaps.push({
      assetSymbol: t.assetSymbol,
      qty: (mv / price).toString(),
      marketValue: mv.toString(),
      price: price.toString(),
    });
  }

  // Preserve any assets that were on the snapshot but not in the normalized targets (e.g. dust);
  // they keep their current state and are not rewritten.
  const knownAssets = new Set(newSnaps.map((s) => s.assetSymbol));
  for (const s of snaps) {
    if (!knownAssets.has(s.assetSymbol)) {
      newSnaps.push({
        assetSymbol: s.assetSymbol,
        qty: s.qty.toString(),
        marketValue: s.marketValue.toString(),
        price: s.price.toString(),
      });
    }
  }

  const mode = workerExecutionMode();
  // In plan_only mode we honestly emulate the snapshot side; in rpc mode the real worker would do this via fills.
  // We still rewrite the snapshot here so the UI demo path closes the loop even before a real Silvana flow is wired.
  await replaceLatestPositionSnapshots(portfolioId, newSnaps);

  await appendAuditLog({
    actorId: actorId(req),
    action: "portfolio.rebalance.now",
    entityType: "portfolio",
    entityId: portfolioId,
    payload: { mode, transfersCount: transfers.length, nav: result.nav },
  });

  res.json({
    portfolioId,
    mode, // "plan_only" | "rpc"
    isDemo: mode === "plan_only",
    nav: result.nav,
    transfers,
    plannedOrdersCount: result.plannedOrders.length,
  });
});
