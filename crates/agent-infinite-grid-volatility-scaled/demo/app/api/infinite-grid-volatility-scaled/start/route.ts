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
      levels: num(body.levels, "levels"),
      quantityPerLevel: num(body.quantityPerLevel, "quantityPerLevel"),
      volWindow: num(body.volWindow, "volWindow"),
      stepMultiplier: num(body.stepMultiplier, "stepMultiplier"),
      minStepPct: num(body.minStepPct, "minStepPct"),
      maxStepPct: num(body.maxStepPct, "maxStepPct"),
      refreshSecs: num(body.refreshSecs, "refreshSecs"),
      startingPrice: num(body.startingPrice, "startingPrice"),
    };
    if (!Number.isInteger(config.levels) || config.levels < 1) return NextResponse.json({ error: "levels must be an integer >= 1" }, { status: 400 });
    if (config.quantityPerLevel <= 0) return NextResponse.json({ error: "quantityPerLevel must be > 0" }, { status: 400 });
    if (!Number.isInteger(config.volWindow) || config.volWindow < 3) return NextResponse.json({ error: "volWindow must be integer >= 3" }, { status: 400 });
    if (config.stepMultiplier <= 0) return NextResponse.json({ error: "stepMultiplier must be > 0" }, { status: 400 });
    if (config.minStepPct <= 0) return NextResponse.json({ error: "minStepPct must be > 0" }, { status: 400 });
    if (config.maxStepPct <= config.minStepPct) return NextResponse.json({ error: "maxStepPct must be > minStepPct" }, { status: 400 });
    if (config.refreshSecs <= 0) return NextResponse.json({ error: "refreshSecs must be > 0" }, { status: 400 });
    if (config.startingPrice <= 0) return NextResponse.json({ error: "startingPrice must be > 0" }, { status: 400 });
    const state = startInfiniteGrid(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
