import { prisma } from "../client.js";
import type { Prisma } from "@prisma/client";

export async function findRebalanceJobById(id: string) {
  return prisma.rebalanceJob.findUnique({
    where: { id },
  });
}

export async function findRebalanceJobWithBatches(id: string) {
  return prisma.rebalanceJob.findUnique({
    where: { id },
    include: {
      orderBatches: {
        select: {
          id: true,
          status: true,
          venue: true,
          market: true,
          submittedOrders: true,
          filledOrders: true,
        },
        orderBy: { createdAt: "asc" },
      },
    },
  });
}

/** Детальный снимок для UI мониторинга: батчи, ордера, последние settlement-события. */
export async function findRebalanceJobWithExecutionDetail(id: string) {
  return prisma.rebalanceJob.findUnique({
    where: { id },
    include: {
      orderBatches: {
        orderBy: { createdAt: "asc" },
        include: {
          orders: {
            orderBy: { createdAt: "asc" },
          },
        },
      },
      settlementEvents: {
        orderBy: { createdAt: "desc" },
        take: 80,
      },
    },
  });
}

/** Мелкая заплатка поверх сохранённого JSON `output` (без затирания всего блока). */
export async function patchRebalanceJobOutput(id: string, patch: Record<string, unknown>): Promise<void> {
  const row = await prisma.rebalanceJob.findUnique({ where: { id }, select: { output: true } });
  const prevRaw = row?.output;
  const prev =
    typeof prevRaw === "object" && prevRaw !== null && !Array.isArray(prevRaw)
      ? (prevRaw as Record<string, unknown>)
      : {};

  await prisma.rebalanceJob.update({
    where: { id },
    data: { output: { ...prev, ...patch } as Prisma.InputJsonValue },
  });
}

export async function updateRebalanceJob(
  id: string,
  data: Readonly<{
    status?: string;
    output?: Prisma.InputJsonValue;
    startedAt?: Date | null;
    finishedAt?: Date | null;
  }>,
): Promise<{ id: string; status: string; dryRun: boolean; finishedAt: Date | null }> {
  return prisma.rebalanceJob.update({
    where: { id },
    data: {
      ...(data.status !== undefined ? { status: data.status } : {}),
      ...(data.output !== undefined ? { output: data.output } : {}),
      ...(data.startedAt !== undefined ? { startedAt: data.startedAt } : {}),
      ...(data.finishedAt !== undefined ? { finishedAt: data.finishedAt } : {}),
    },
    select: {
      id: true,
      status: true,
      dryRun: true,
      finishedAt: true,
    },
  });
}

export async function createRebalanceJob(
  params: Readonly<{
    portfolioId: string;
    status: string;
    mode: string;
    requestedBy?: string | null;
    dryRun: boolean;
    input: Prisma.InputJsonValue;
    output?: Prisma.InputJsonValue;
  }>,
): Promise<{ id: string }> {
  return prisma.rebalanceJob.create({
    data: {
      portfolioId: params.portfolioId,
      status: params.status,
      mode: params.mode,
      requestedBy: params.requestedBy ?? undefined,
      dryRun: params.dryRun,
      input: params.input,
      output: params.output,
    },
    select: { id: true },
  });
}
