import { Router } from "express";
import type { Prisma } from "@prisma/client";
import {
  analyzeRebalance,
  checkDailyEstimatedNotional,
  checkPlannedOrdersMaxNotional,
  parseRiskMaxDailyNotionalFromEnv,
  parseRiskMaxOrderNotionalFromEnv,
} from "@batch-order/portfolio-engine";
import {
  appendAuditLog,
  createRebalanceJob,
  findPortfolioById,
  findRebalanceJobWithExecutionDetail,
  findRebalanceJobWithBatches,
  listLatestSnapshotsForPortfolio,
  sumCompletedLiveEstimatedNotionalUtcDay,
  updateRebalanceJob,
} from "@batch-order/db";
import { isUuid } from "../lib/ids.js";
import { actorId } from "../lib/request-context.js";
import {
  decodeTargets,
  parseThresholdBps,
  slipBpsFromEnv,
  snapshotsToPositions,
  targetsFromPortfolio,
} from "../lib/rebalance-input.js";
import { COMMAND_JOB_NAMES, getCommandsQueue } from "../lib/commands-queue.js";

export const rebalanceRouter = Router();

function envThresholdBps(): number {
  const tb = Number(process.env.REBALANCE_THRESHOLD_BPS ?? "100");
  return Number.isFinite(tb) && tb >= 0 ? tb : 100;
}

function jsonSerializable<T>(value: T): Prisma.InputJsonValue {
  return JSON.parse(JSON.stringify(value)) as Prisma.InputJsonValue;
}

rebalanceRouter.post("/preview", async (req, res) => {
  try {
    const body = req.body as Record<string, unknown>;
    const portfolioId = typeof body.portfolioId === "string" ? body.portfolioId : "";
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

    const thresholdBps = parseThresholdBps(body, envThresholdBps());
    const decoded = decodeTargets(body.targets);
    const targets = decoded && decoded.length > 0 ? decoded : targetsFromPortfolio(portfolio.targets);
    if (targets.length === 0) {
      res.status(400).json({ error: "targets_empty" });
      return;
    }

    const slip = slipBpsFromEnv();
    const result = analyzeRebalance({
      portfolioId,
      quoteCurrency: portfolio.baseCurrency,
      thresholdBps,
      targets,
      positions: snapshotsToPositions(snaps),
      slipBpsBuy: slip.buy,
      slipBpsSell: slip.sell,
    });

    const riskForLive: {
      liveExecuteAllowed: boolean;
      violations: Array<{ code: string; message: string }>;
    } = { liveExecuteAllowed: true, violations: [] };
    const maxOrderL = parseRiskMaxOrderNotionalFromEnv();
    const orderV = checkPlannedOrdersMaxNotional(result.plannedOrders, maxOrderL);
    if (orderV) riskForLive.violations.push(orderV);
    const maxDayL = parseRiskMaxDailyNotionalFromEnv();
    if (maxDayL) {
      const used = await sumCompletedLiveEstimatedNotionalUtcDay(portfolioId);
      const dayV = checkDailyEstimatedNotional(result.estimatedNotional, used.toString(), maxDayL);
      if (dayV) riskForLive.violations.push(dayV);
    }
    riskForLive.liveExecuteAllowed = riskForLive.violations.length === 0;

    const inputStored = jsonSerializable({
      kind: "preview" as const,
      portfolioId,
      thresholdBps,
      targets: targets.map((t) => ({
        assetSymbol: t.assetSymbol,
        weight: typeof t.weight === "number" ? t.weight : String(t.weight),
        enabled: t.enabled ?? true,
      })),
    });

    const { id: previewId } = await createRebalanceJob({
      portfolioId,
      status: "completed",
      mode: "preview",
      requestedBy: actorId(req),
      dryRun: true,
      input: inputStored,
      output: jsonSerializable(result),
    });

    await appendAuditLog({
      actorId: actorId(req),
      action: "rebalance.preview",
      entityType: "rebalance_job",
      entityId: previewId,
      payload: { portfolioId },
    });

    res.json({
      previewId,
      current: result.current.map((x) => ({ assetSymbol: x.assetSymbol, weight: x.weight })),
      drift: result.drift.map((d) => ({
        assetSymbol: d.assetSymbol,
        targetWeight: d.targetWeight,
        currentWeight: d.currentWeight,
        driftWeightBps: d.driftWeightBps,
        skippedDueToThreshold: d.skippedDueToThreshold,
      })),
      normalizedTargets: result.normalizedTargets.map((x) => ({
        assetSymbol: x.assetSymbol,
        weight: x.weight,
      })),
      nav: result.nav,
      warnings: result.warnings,
      plannedOrders: result.plannedOrders.map((o) => ({
        market: o.market,
        assetSymbol: o.assetSymbol,
        side: o.side,
        qty: o.qty,
        type: o.type,
        price: o.price,
        ...(o.venue !== undefined ? { venue: o.venue } : {}),
        ...(o.execProfile !== undefined ? { execProfile: o.execProfile } : {}),
      })),
      estimatedNotional: result.estimatedNotional,
      riskForLive,
    });
  } catch (err) {
    res.status(400).json({
      error: "preview_failed",
      message: err instanceof Error ? err.message : "unknown_error",
    });
  }
});

rebalanceRouter.post("/execute", async (req, res) => {
  const body = req.body as Record<string, unknown>;
  const portfolioId = typeof body.portfolioId === "string" ? body.portfolioId : "";

  try {
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

    const thresholdBps = parseThresholdBps(body, envThresholdBps());
    const decoded = decodeTargets(body.targets);
    const targets = decoded && decoded.length > 0 ? decoded : targetsFromPortfolio(portfolio.targets);
    if (targets.length === 0) {
      res.status(400).json({ error: "targets_empty" });
      return;
    }

    const dryRunRaw = body.dryRun;
    const dryRun = typeof dryRunRaw === "boolean" ? dryRunRaw : String(dryRunRaw).toLowerCase() === "true";

    const slip = slipBpsFromEnv();
    const positions = snapshotsToPositions(snaps);
    const analyzed = analyzeRebalance({
      portfolioId,
      quoteCurrency: portfolio.baseCurrency,
      thresholdBps,
      targets,
      positions,
      slipBpsBuy: slip.buy,
      slipBpsSell: slip.sell,
    });

    if (!dryRun) {
      const maxOrder = parseRiskMaxOrderNotionalFromEnv();
      const orderFail = checkPlannedOrdersMaxNotional(analyzed.plannedOrders, maxOrder);
      if (orderFail) {
        await appendAuditLog({
          actorId: actorId(req),
          action: "rebalance.execute.risk_blocked",
          entityType: "portfolio",
          entityId: portfolioId,
          payload: { code: orderFail.code, message: orderFail.message },
        });
        res.status(403).json({ error: orderFail.code, message: orderFail.message });
        return;
      }
      const maxDaily = parseRiskMaxDailyNotionalFromEnv();
      if (maxDaily) {
        const used = await sumCompletedLiveEstimatedNotionalUtcDay(portfolioId);
        const dayFail = checkDailyEstimatedNotional(analyzed.estimatedNotional, used.toString(), maxDaily);
        if (dayFail) {
          await appendAuditLog({
            actorId: actorId(req),
            action: "rebalance.execute.risk_blocked",
            entityType: "portfolio",
            entityId: portfolioId,
            payload: { code: dayFail.code, message: dayFail.message },
          });
          res.status(403).json({ error: dayFail.code, message: dayFail.message });
          return;
        }
      }
    }

    const inputStored = jsonSerializable({
      kind: "execute" as const,
      portfolioId,
      thresholdBps,
      targets: targets.map((t) => ({
        assetSymbol: t.assetSymbol,
        weight: typeof t.weight === "number" ? t.weight : String(t.weight),
        enabled: t.enabled ?? true,
      })),
      dryRun,
      positions,
    });

    const { id: jobId } = await createRebalanceJob({
      portfolioId,
      status: "queued",
      mode: dryRun ? "dry_run_execute" : "live_execute",
      requestedBy: actorId(req),
      dryRun,
      input: inputStored,
    });

    await appendAuditLog({
      actorId: actorId(req),
      action: "rebalance.execute.enqueued",
      entityType: "rebalance_job",
      entityId: jobId,
      payload: { portfolioId, dryRun },
    });

    try {
      const { queue } = getCommandsQueue();
      await queue.add(COMMAND_JOB_NAMES.REBALANCE_EXECUTE, { rebalanceJobId: jobId });
    } catch (err) {
      await updateRebalanceJob(jobId, {
        status: "failed",
        finishedAt: new Date(),
        output: jsonSerializable({
          phase: "enqueue",
          error: err instanceof Error ? err.message : "queue_error",
        }),
      });

      await appendAuditLog({
        actorId: actorId(req),
        action: "rebalance.execute.enqueue_failed",
        entityType: "rebalance_job",
        entityId: jobId,
        payload: {
          portfolioId,
          error: err instanceof Error ? err.message : "queue_error",
        },
      });

      res.status(503).json({ error: "queue_unavailable" });
      return;
    }

    res.json({ jobId, status: "queued" });
  } catch (err) {
    res.status(400).json({
      error: "execute_failed",
      message: err instanceof Error ? err.message : "unknown_error",
    });
  }
});

rebalanceRouter.get("/jobs/:id", async (req, res) => {
  const jobId = req.params.id ?? "";
  if (!isUuid(jobId)) {
    res.status(400).json({ error: "jobId_invalid" });
    return;
  }

  const detailed =
    req.query.detail === "1" ||
    req.query.detail === "true" ||
    req.query.detail === "yes";

  if (detailed) {
    const job = await findRebalanceJobWithExecutionDetail(jobId);

    if (!job) {
      res.status(404).json({ error: "job_not_found" });
      return;
    }

    res.json({
      jobId: job.id,

      portfolioId: job.portfolioId,
      status: job.status,
      mode: job.mode,
      dryRun: job.dryRun,
      requestedBy: job.requestedBy,
      input: job.input,
      output: job.output,
      startedAt: job.startedAt?.toISOString() ?? null,
      finishedAt: job.finishedAt?.toISOString() ?? null,
      createdAt: job.createdAt.toISOString(),
      batches: job.orderBatches.map((b) => ({
        batchId: b.id,
        status: b.status,
        venue: b.venue,
        market: b.market,

        totalOrders: b.totalOrders,
        submittedOrders: b.submittedOrders,
        filledOrders: b.filledOrders,
        cancelledOrders: b.cancelledOrders,

        orders: b.orders.map((o) => ({
          orderId: o.id,

          venue: o.venue,
          venueOrderRef: o.venueOrderRef,
          execProfile: o.execProfile,

          silvanaOrderId: o.silvanaOrderId,

          side: o.side,

          type: o.type,

          market: o.market,

          price: o.price?.toString() ?? null,

          qty: o.qty.toString(),

          status: o.status,

          clientOrderRef: o.clientOrderRef,

          createdAt: o.createdAt.toISOString(),
          updatedAt: o.updatedAt.toISOString(),
        })),
      })),
      settlements: job.settlementEvents.map((s) => ({
        id: s.id,
        status: s.status,

        payload: s.payload,

        createdAt: s.createdAt.toISOString(),
      })),
    });
    return;
  }

  const job = await findRebalanceJobWithBatches(jobId);
  if (!job) {
    res.status(404).json({ error: "job_not_found" });
    return;
  }

  res.json({
    jobId: job.id,

    status: job.status,
    batches: job.orderBatches.map((b) => ({
      batchId: b.id,

      status: b.status,

      venue: b.venue,
      submittedOrders: b.submittedOrders,
      filledOrders: b.filledOrders,
    })),
  });
});
