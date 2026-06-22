import { Prisma } from "@prisma/client";
import { prisma } from "../client.js";

/** При дубликате `silvanaStreamKey` возвращает существующую строку (ingest subscribeSettlements). */
export async function appendSettlementEvent(
  params: Readonly<{ rebalanceJobId: string; payload: Prisma.InputJsonValue; status?: string | null; silvanaStreamKey?: string | null }>,
) {
  try {
    return await prisma.settlementEvent.create({
      data: {
        rebalanceJobId: params.rebalanceJobId,
        payload: params.payload,
        status: params.status ?? undefined,
        silvanaStreamKey: params.silvanaStreamKey ?? undefined,
      },
    });
  } catch (e: unknown) {
    if (e instanceof Prisma.PrismaClientKnownRequestError && e.code === "P2002" && params.silvanaStreamKey) {
      const existing = await prisma.settlementEvent.findUnique({
        where: { silvanaStreamKey: params.silvanaStreamKey },
      });
      if (existing) return existing;
    }
    throw e;
  }
}
