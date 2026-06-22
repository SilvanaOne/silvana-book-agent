import type { RequestHandler } from "express";

// Этап 10 (секреты): MCP и venue ключи — только worker или отдельный gateway; не `NEXT_PUBLIC_*` (видны в бандле).

function pathWithoutQuery(originalUrl: string): string {
  return originalUrl.split("?")[0] ?? originalUrl;
}

/**
 * Если задан `API_INTERNAL_KEY`, требует `Authorization: Bearer <key>` или заголовок `x-api-key` для любых `/api/**` запросов (сервер Express).
 */
export const internalAuthForApi: RequestHandler = (req, res, next) => {
  const key = process.env.API_INTERNAL_KEY?.trim();
  if (!key) {
    next();
    return;
  }

  const pathname = pathWithoutQuery(req.originalUrl);
  if (!pathname.startsWith("/api")) {
    next();
    return;
  }

  const authHeader = typeof req.headers.authorization === "string" ? req.headers.authorization.trim() : "";
  const bearer = authHeader.toLowerCase().startsWith("bearer ") ? authHeader.slice("bearer ".length).trim() : "";

  const xApi = typeof req.headers["x-api-key"] === "string" ? req.headers["x-api-key"].trim() : "";

  const provided = bearer.length > 0 ? bearer : xApi;
  if (!provided || provided !== key) {
    res.status(401).json({ error: "unauthorized" });
    return;
  }
  next();
};
