import type { Prisma } from "@prisma/client";
import { prisma } from "../client.js";

const TERMINAL_FOR_BATCH = new Set(["filled", "cancelled", "expired"]);

const FINAL_BATCH_ROW = new Set(["completed", "cancelled", "failed"]);

async function patchRebalanceJobOutputTx(tx: Prisma.TransactionClient, id: string, patch: Record<string, unknown>): Promise<void> {
  const row = await tx.rebalanceJob.findUnique({ where: { id }, select: { output: true } });
  const prevRaw = row?.output;
  const prev =
    typeof prevRaw === "object" && prevRaw !== null && !Array.isArray(prevRaw)
      ? (prevRaw as Record<string, unknown>)
      : {};
  await tx.rebalanceJob.update({
    where: { id },
    data: { output: { ...prev, ...patch } as Prisma.InputJsonValue },
  });
}

export type IngestOrderStreamResult = "applied" | "duplicate" | "unknown_order";

/**
 * Одна мутация из subscribeOrders / reconcile: статус строки ордера, order_events (+ dedup по silvanaEventKey),
 * счётчики батча, при необходимости завершение батча и пометка job output.
 */
export async function ingestOrderLifecycleFromSilvanaStream(params: Readonly<{
  silvanaOrderId: string;
  eventTypeLabel: string;
  payload: Prisma.InputJsonValue;
  silvanaEventKey?: string | null;
  domesticStatusFromBook: string;
}>): Promise<IngestOrderStreamResult> {
  return prisma.$transaction(async (tx) => {
    if (params.silvanaEventKey) {
      const exists = await tx.orderEvent.findUnique({
        where: { silvanaEventKey: params.silvanaEventKey },
        select: { id: true },
      });
      if (exists) return "duplicate";
    }

    const order = await tx.order.findUnique({
      where: { silvanaOrderId: params.silvanaOrderId },
      select: {
        id: true,
        orderBatchId: true,
        status: true,
      },
    });
    if (!order) return "unknown_order";

    const prev = order.status;
    const next = params.domesticStatusFromBook;

    const batchRow = await tx.orderBatch.findUnique({
      where: { id: order.orderBatchId },
      select: { id: true, rebalanceJobId: true, status: true },
    });
    if (!batchRow) return "unknown_order";

    let incrFilled = false;
    let incrCancelled = false;

    if (prev !== next) {
      if (next === "filled" && prev !== "filled") incrFilled = true;
      const becameTerminalCancel =
        (next === "cancelled" || next === "expired") &&
        prev !== "cancelled" &&
        prev !== "expired" &&
        prev !== "filled";

      if (becameTerminalCancel) incrCancelled = true;

      await tx.order.update({
        where: { id: order.id },
        data: { status: next },
      });
    }

    await tx.orderEvent.create({
      data: {
        orderId: order.id,
        eventType: params.eventTypeLabel,
        payload: params.payload,
        silvanaEventKey: params.silvanaEventKey ?? undefined,
      },
    });

    if (incrFilled || incrCancelled) {
      await tx.orderBatch.update({
        where: { id: order.orderBatchId },
        data: {
          ...(incrFilled ? { filledOrders: { increment: 1 } } : {}),
          ...(incrCancelled ? { cancelledOrders: { increment: 1 } } : {}),
        },
      });
    }

    if (
      batchRow.status !== "failed" &&
      batchRow.status !== "cancelled" &&
      !FINAL_BATCH_ROW.has(batchRow.status)
    ) {
      const legs = await tx.order.findMany({
        where: { orderBatchId: order.orderBatchId },
        select: { status: true },
      });

      const allExecDone = legs.length > 0 && legs.every((l) => TERMINAL_FOR_BATCH.has(l.status));
      if (allExecDone && batchRow.status === "pending") {
        await tx.orderBatch.update({
          where: { id: order.orderBatchId },
          data: { status: "completed" },
        });
      }

      const batches = await tx.orderBatch.findMany({
        where: { rebalanceJobId: batchRow.rebalanceJobId },
        select: { status: true },
      });
      if (batches.length > 0 && batches.every((b) => FINAL_BATCH_ROW.has(b.status))) {
        await patchRebalanceJobOutputTx(tx, batchRow.rebalanceJobId, {
          streamPhase: "all_batches_terminal",
          streamSyncedAt: new Date().toISOString(),
        });
      }
    }

    return "applied";
  });
}
