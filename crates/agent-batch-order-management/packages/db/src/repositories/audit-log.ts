import { prisma } from "../client.js";
import type { Prisma } from "@prisma/client";

export async function appendAuditLog(
  params: Readonly<{ actorId: string; action: string; entityType: string; entityId: string; payload: Prisma.InputJsonValue }>,
) {
  return prisma.auditLog.create({
    data: {
      actorId: params.actorId,
      action: params.action,
      entityType: params.entityType,
      entityId: params.entityId,
      payload: params.payload,
    },
  });
}

export async function listRecentAuditLogs(params: Readonly<{ take: number; beforeCreatedAt?: Date }>) {
  const take = params.take;

  return prisma.auditLog.findMany({
    where: params.beforeCreatedAt ? { createdAt: { lt: params.beforeCreatedAt } } : undefined,

    orderBy: { createdAt: "desc" },
    take,
  });
}
