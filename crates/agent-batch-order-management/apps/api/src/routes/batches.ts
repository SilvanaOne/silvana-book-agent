import { Router } from "express";
import { appendAuditLog, findOrderBatchById } from "@batch-order/db";
import { isUuid } from "../lib/ids.js";
import { actorId } from "../lib/request-context.js";
import { COMMAND_JOB_NAMES, getCommandsQueue } from "../lib/commands-queue.js";

export const batchesRouter = Router();

batchesRouter.post("/:id/cancel", async (req, res) => {
  const batchId = req.params.id ?? "";
  if (!isUuid(batchId)) {
    res.status(400).json({ error: "batchId_invalid" });
    return;
  }

  const batch = await findOrderBatchById(batchId);
  if (!batch) {
    res.status(404).json({ error: "batch_not_found" });
    return;
  }

  try {
    const { queue } = getCommandsQueue();
    await queue.add(COMMAND_JOB_NAMES.BATCH_CANCEL, { orderBatchId: batchId });
  } catch (err) {
    res.status(503).json({ error: "queue_unavailable", message: err instanceof Error ? err.message : undefined });
    return;
  }

  await appendAuditLog({
    actorId: actorId(req),
    action: "batch.cancel.enqueued",
    entityType: "order_batch",
    entityId: batchId,
    payload: { rebalanceJobId: batch.rebalanceJobId },
  });

  res.json({ ok: true });
});
