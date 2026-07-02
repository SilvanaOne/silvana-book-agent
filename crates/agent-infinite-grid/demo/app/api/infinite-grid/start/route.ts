import { NextResponse } from "next/server";
import { startInfiniteGrid } from "@/lib/store";
import type { InfiniteGridConfig } from "@/lib/infinitegrid-engine";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function num(v: unknown, name: string): number {
  const n = typeof v === "number" ? v : Number(v);
  if (!Number.isFinite(n)) throw new Error(`${name} must be a finite number`);
  return n;
}

export async function POST(req: Request): Promise<Response> {
  const body = (await req.json()) as Record<string, unknown>;
  try {
    const config: InfiniteGridConfig = {
      market: String(body.market ?? "CC-USDC"),
      stepPct: num(body.stepPct, "stepPct"),
      levels: num(body.levels, "levels"),
      quantityPerLevel: num(body.quantityPerLevel, "quantityPerLevel"),
      refreshSecs: num(body.refreshSecs, "refreshSecs"),
      driftThresholdPct: num(body.driftThresholdPct, "driftThresholdPct"),
      startingPrice: num(body.startingPrice, "startingPrice"),
    };
    if (config.stepPct <= 0) return NextResponse.json({ error: "stepPct must be > 0" }, { status: 400 });
    if (!Number.isInteger(config.levels) || config.levels < 1) return NextResponse.json({ error: "levels must be an integer >= 1" }, { status: 400 });
    if (config.quantityPerLevel <= 0) return NextResponse.json({ error: "quantityPerLevel must be > 0" }, { status: 400 });
    if (config.refreshSecs <= 0) return NextResponse.json({ error: "refreshSecs must be > 0" }, { status: 400 });
    if (config.driftThresholdPct < 0) return NextResponse.json({ error: "driftThresholdPct must be >= 0" }, { status: 400 });
    if (config.startingPrice <= 0) return NextResponse.json({ error: "startingPrice must be > 0" }, { status: 400 });
    const state = startInfiniteGrid(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
