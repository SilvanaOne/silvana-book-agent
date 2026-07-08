import { NextResponse } from "next/server";
import { startHedging } from "@/lib/store";
import type { HedgingConfig } from "@/lib/hedging-engine";

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
    const config: HedgingConfig = {
      exposureInstrument: String(body.exposureInstrument ?? "Amulet"),
      hedgeMarket: String(body.hedgeMarket ?? "CC-USDC"),
      targetBalance: num(body.targetBalance, "targetBalance"),
      tolerance: num(body.tolerance, "tolerance"),
      hedgeFraction: num(body.hedgeFraction, "hedgeFraction"),
      checkIntervalSecs: num(body.checkIntervalSecs, "checkIntervalSecs"),
      exposureDriftPerTick: num(body.exposureDriftPerTick, "exposureDriftPerTick"),
      startingBalance: num(body.startingBalance, "startingBalance"),
      startingPrice: num(body.startingPrice, "startingPrice"),
    };
    if (config.targetBalance <= 0) return NextResponse.json({ error: "targetBalance must be > 0" }, { status: 400 });
    if (config.tolerance < 0) return NextResponse.json({ error: "tolerance must be >= 0" }, { status: 400 });
    if (config.hedgeFraction <= 0 || config.hedgeFraction > 1) return NextResponse.json({ error: "hedgeFraction must be in (0, 1]" }, { status: 400 });
    if (config.checkIntervalSecs <= 0) return NextResponse.json({ error: "checkIntervalSecs must be > 0" }, { status: 400 });
    if (config.exposureDriftPerTick < 0) return NextResponse.json({ error: "exposureDriftPerTick must be >= 0" }, { status: 400 });
    if (config.startingBalance < 0) return NextResponse.json({ error: "startingBalance must be >= 0" }, { status: 400 });
    if (config.startingPrice <= 0) return NextResponse.json({ error: "startingPrice must be > 0" }, { status: 400 });
    const state = startHedging(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
