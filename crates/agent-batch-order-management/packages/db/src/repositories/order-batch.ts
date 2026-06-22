import type { Prisma } from "@prisma/client";
import { prisma } from "../client.js";

export async function createOrderBatch(
  params: Readonly<{ rebalanceJobId: string; status: string; venue?: string; market: string }>,
): Promise<{ id: string }> {
  return prisma.orderBatch.create({
    data: {
      rebalanceJobId: params.rebalanceJobId,
      status: params.status,
      venue: params.venue ?? "silvana",
      market: params.market,
      totalOrders: 0,
    },
    select: { id: true },
  });
}

export async function findOrderBatchById(id: string) {
  return prisma.orderBatch.findUnique({
    where: { id },
    select: {
      id: true,
      rebalanceJobId: true,
      status: true,
      venue: true,
      market: true,
    },
  });
}

export async function listOrderBatchesByRebalanceJob(rebalanceJobId: string) {


  return prisma.orderBatch.findMany({
    where: { rebalanceJobId },

    orderBy: { createdAt: "asc" },

    select: {
      id: true,

      rebalanceJobId: true,

      status: true,
      venue: true,
      market: true,
    },
  });
}

export async function updateOrderBatch(id: string, data: Prisma.OrderBatchUpdateInput) {
  return prisma.orderBatch.update({
    where: { id },
    data,
    select: {
      id: true,
      status: true,
    },

  });


}
