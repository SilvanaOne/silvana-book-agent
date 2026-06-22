import { Prisma } from "@prisma/client";
import { prisma } from "../client.js";

/** При дубликате `silvanaEventKey` возвращает существующую строку (идемпотентность ingest стрима). */
export async function appendOrderEvent(
  params: Readonly<{
    orderId: string;
    eventType: string;
    payload: Prisma.InputJsonValue;
    silvanaEventKey?: string | null;
  }>,
) {
  try {
    return await prisma.orderEvent.create({
      data: {
        orderId: params.orderId,
        eventType: params.eventType,
        payload: params.payload,
        silvanaEventKey: params.silvanaEventKey ?? undefined,
      },
    });
  } catch (e: unknown) {
    if (
      e instanceof Prisma.PrismaClientKnownRequestError &&
      e.code === "P2002" &&
      params.silvanaEventKey
    ) {
      const existing = await prisma.orderEvent.findUnique({
        where: { silvanaEventKey: params.silvanaEventKey },
      });
      if (existing) return existing;
    }
    throw e;
  }
}
