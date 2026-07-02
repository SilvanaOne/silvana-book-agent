import { NextResponse } from "next/server";
import { startLiquidityScreening } from "@/lib/store";
import type { LiquidityScreeningConfig } from "@/lib/liquidityscreening-engine";

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
    const config: LiquidityScreeningConfig = {
      market: String(body.market ?? "CC-USDC"),
      probeQty: num(body.probeQty, "probeQty"),
      depth: num(body.depth, "depth"),
      pollSecs: num(body.pollSecs, "pollSecs"),
      spreadBps: num(body.spreadBps, "spreadBps"),
      depthFalloffPct: num(body.depthFalloffPct, "depthFalloffPct"),
      startingPrice: num(body.startingPrice, "startingPrice"),
    };
    if (config.probeQty <= 0) return NextResponse.json({ error: "probeQty must be > 0" }, { status: 400 });
    if (config.depth < 1) return NextResponse.json({ error: "depth must be >= 1" }, { status: 400 });
    if (config.pollSecs <= 0) return NextResponse.json({ error: "pollSecs must be > 0" }, { status: 400 });
    if (config.spreadBps <= 0) return NextResponse.json({ error: "spreadBps must be > 0" }, { status: 400 });
    if (config.depthFalloffPct <= 0) return NextResponse.json({ error: "depthFalloffPct must be > 0" }, { status: 400 });
    if (config.startingPrice <= 0) return NextResponse.json({ error: "startingPrice must be > 0" }, { status: 400 });
    const state = startLiquidityScreening(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
