import { Router } from "express";
import { listRecentAuditLogs } from "@batch-order/db";

export const auditRouter = Router();

auditRouter.get("/logs", async (req, res) => {
  const raw = Number(req.query.limit ?? 100);
  const limit = Number.isFinite(raw) ? Math.min(Math.max(raw, 1), 500) : 100;

  const logs = await listRecentAuditLogs({ take: limit });

  res.json({
    logs: logs.map((l) => ({
      id: l.id,
      actorId: l.actorId,
      action: l.action,
      entityType: l.entityType,
      entityId: l.entityId,
      payload: l.payload,
      createdAt: l.createdAt.toISOString(),
    })),
  });
});
