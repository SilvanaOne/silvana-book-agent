import { NextResponse } from "next/server";
import { startAgent } from "@/lib/store";
import type { AnomalyConfig } from "@/lib/anomaly-engine";

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
    const historyDays = Math.max(1, Math.min(14, Math.floor(num(body.historyDays, "historyDays"))));
    const recordsPerDay = Math.max(20, Math.min(500, Math.floor(num(body.recordsPerDay, "recordsPerDay"))));
    const markets = parseList(body.markets, true);
    if (markets.length === 0) throw new Error("markets pool required");
    const settlementWindowSecs = Math.max(1, Math.floor(num(body.settlementWindowSecs, "settlementWindowSecs")));
    const rapidCancelWindowMs = Math.max(1, Math.floor(num(body.rapidCancelWindowMs, "rapidCancelWindowMs")));
    const layerThreshold = Math.max(2, Math.floor(num(body.layerThreshold, "layerThreshold")));
    const layerBandPct = num(body.layerBandPct, "layerBandPct");
    if (layerBandPct <= 0) throw new Error("layerBandPct > 0");
    const burstCount = Math.max(2, Math.floor(num(body.burstCount, "burstCount")));
    const burstWindowSecs = Math.max(1, Math.floor(num(body.burstWindowSecs, "burstWindowSecs")));
    const injectAnomalies = Boolean(body.injectAnomalies);

    const config: AnomalyConfig = {
      historyDays, recordsPerDay, markets,
      settlementWindowSecs, rapidCancelWindowMs, layerThreshold, layerBandPct,
      burstCount, burstWindowSecs, injectAnomalies,
    };
    const state = startAgent(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
