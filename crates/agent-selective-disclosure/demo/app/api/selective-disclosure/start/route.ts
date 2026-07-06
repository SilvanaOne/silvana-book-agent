import { NextResponse } from "next/server";
import { startAgent } from "@/lib/store";
import type { DiscloseConfig, Kind } from "@/lib/disclose-engine";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function num(v: unknown, name: string): number {
  const n = typeof v === "number" ? v : Number(v);
  if (!Number.isFinite(n)) throw new Error(`${name} must be a finite number`);
  return n;
}

const VALID_KINDS: Kind[] = ["order.created", "order.filled", "order.cancelled", "settlement.proposal", "settlement.settled", "settlement.failed"];

function parseKinds(raw: unknown): Kind[] {
  if (typeof raw !== "string" || raw.trim().length === 0) return [];
  const list = raw.split(",").map((s) => s.trim()).filter((s) => s.length > 0) as Kind[];
  for (const k of list) if (!VALID_KINDS.includes(k)) throw new Error(`unknown kind "${k}"`);
  return list;
}

function parseList(raw: unknown, upper = false): string[] {
  if (typeof raw !== "string" || raw.trim().length === 0) return [];
  return raw.split(",").map((s) => s.trim()).filter((s) => s.length > 0).map((s) => upper ? s.toUpperCase() : s);
}

export async function POST(req: Request): Promise<Response> {
  const body = (await req.json()) as Record<string, unknown>;
  try {
    const historySize = Math.max(10, Math.min(200, Math.floor(num(body.historySize, "historySize"))));
    const kinds = parseKinds(body.kinds);
    const markets = parseList(body.markets, true);
    const redactFields = parseList(body.redactFields);
    const parties = parseList(body.parties);
    const marketPool = parseList(body.marketPool, true);
    if (parties.length === 0) throw new Error("parties pool required");
    if (marketPool.length === 0) throw new Error("marketPool required");

    const config: DiscloseConfig = { historySize, kinds, markets, redactFields, parties, marketPool };
    const state = startAgent(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
