import { prisma } from "../client.js";
import type { Prisma } from "@prisma/client";
import { patchRebalanceJobOutput } from "./rebalance-job.js";

export type SettlementIngestOutcome = "applied" | "duplicate";

/**
 * Идempotentная запись `settlement_events` + лёгкое дополнение `rebalance_jobs.output` для мониторинга.
 */
export async function ingestSettlementStreamEvent(params: Readonly<{
  rebalanceJobId: string;
  silvanaStreamKey: string;
  eventStatusLabel: string;
  payload: Prisma.InputJsonValue;
}>): Promise<SettlementIngestOutcome> {
  const existing = await prisma.settlementEvent.findUnique({
    where: { silvanaStreamKey: params.silvanaStreamKey },
    select: { id: true },
  });
  if (existing) return "duplicate";

  await prisma.settlementEvent.create({
    data: {
      rebalanceJobId: params.rebalanceJobId,
      payload: params.payload,
      status: params.eventStatusLabel,
      silvanaStreamKey: params.silvanaStreamKey,
    },
  });

  await patchRebalanceJobOutput(params.rebalanceJobId, {
    lastSettlementStreamAt: new Date().toISOString(),
    lastSettlementStreamStatus: params.eventStatusLabel,
  });

  return "applied";
}
