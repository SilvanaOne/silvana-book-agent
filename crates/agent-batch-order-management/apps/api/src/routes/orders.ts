import { Router } from "express";
import { appendAuditLog, findOrderWithBatch } from "@batch-order/db";
import { isUuid } from "../lib/ids.js";
import { actorId } from "../lib/request-context.js";
import { COMMAND_JOB_NAMES, getCommandsQueue } from "../lib/commands-queue.js";

export const ordersRouter = Router();

ordersRouter.post("/:id/replace", async (req, res) => {
  const orderId = req.params.id ?? "";
  if (!isUuid(orderId)) {
    res.status(400).json({ error: "orderId_invalid" });
    return;
  }

  const body = req.body as Record<string, unknown>;
  if (typeof body.price !== "string" && typeof body.price !== "number") {
    res.status(400).json({ error: "price_invalid" });
    return;
  }
  if (typeof body.qty !== "string" && typeof body.qty !== "number") {
    res.status(400).json({ error: "qty_invalid" });
    return;
  }

  const price = String(body.price);
  const qty = String(body.qty);

  const order = await findOrderWithBatch(orderId);
  if (!order) {
    res.status(404).json({ error: "order_not_found" });
    return;
  }

  try {
    const { queue } = getCommandsQueue();
    await queue.add(COMMAND_JOB_NAMES.ORDER_REPLACE, { orderId, price, qty });
  } catch (err) {
    res.status(503).json({ error: "queue_unavailable", message: err instanceof Error ? err.message : undefined });
    return;
  }

  await appendAuditLog({
    actorId: actorId(req),
    action: "order.replace.enqueued",
    entityType: "order",
    entityId: orderId,
    payload: {
      rebalanceJobId: order.orderBatch.rebalanceJobId,
      batchId: order.orderBatch.id,
      price,
      qty,
    },
  });

  // В фазе 6 сюда придёт Silvana new id; сейчас возвращаем текущий внутренний id как канонический order row.
  res.json({ ok: true, newOrderId: orderId });
});
