import type { Request } from "express";

export function actorId(req: Request): string {
  const raw = typeof req.headers["x-actor-id"] === "string" ? req.headers["x-actor-id"].trim() : "";
  return raw.length > 0 ? raw : "anonymous";
}
