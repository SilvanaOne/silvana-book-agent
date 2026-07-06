import { NextResponse } from "next/server";
import { startAgent } from "@/lib/store";
import type { ClassifierConfig } from "@/lib/classifier-engine";

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
    const windowSecs = Math.max(1, Math.floor(num(body.windowSecs, "windowSecs")));
    const clusterThreshold = Math.max(2, Math.floor(num(body.clusterThreshold, "clusterThreshold")));
    const markets = parseList(body.markets, true);
    if (markets.length === 0) throw new Error("markets required");
    const ratePerSec = num(body.ratePerSec, "ratePerSec");
    if (ratePerSec <= 0 || ratePerSec > 20) throw new Error("ratePerSec in (0, 20]");
    const injectSpoofing = Boolean(body.injectSpoofing);
    const injectWashTrading = Boolean(body.injectWashTrading);

    const config: ClassifierConfig = { windowSecs, clusterThreshold, markets, ratePerSec, injectSpoofing, injectWashTrading };
    const state = startAgent(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
