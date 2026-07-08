import { NextResponse } from "next/server";
import { startAgent } from "@/lib/store";
import type { ExplainConfig } from "@/lib/explain-engine";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function num(v: unknown, name: string): number {
  const n = typeof v === "number" ? v : Number(v);
  if (!Number.isFinite(n)) throw new Error(`${name} must be a finite number`);
  return n;
}
function parseList(raw: unknown, upper = false): string[] {
  if (typeof raw !== "string") return [];
  return raw.split(",").map((s) => s.trim()).filter((s) => s.length > 0).map((s) => upper ? s.toUpperCase() : s);
}

export async function POST(req: Request): Promise<Response> {
  const body = (await req.json()) as Record<string, unknown>;
  try {
    const historySize = Math.max(10, Math.min(200, Math.floor(num(body.historySize, "historySize"))));
    const markets = parseList(body.markets, true);
    if (markets.length === 0) throw new Error("markets required");
    const includeOrders = Boolean(body.includeOrders);
    const includeSettlements = Boolean(body.includeSettlements);
    const model = typeof body.model === "string" && body.model.trim().length > 0 ? body.model.trim() : "silvana-mock-explain-v1";
    const config: ExplainConfig = { historySize, markets, includeOrders, includeSettlements, model };
    const state = startAgent(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
