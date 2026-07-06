import { NextResponse } from "next/server";
import { startAgent } from "@/lib/store";
import type { InvRiskConfig } from "@/lib/invrisk-engine";

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
    const instrument = typeof body.instrument === "string" ? body.instrument.trim() : "";
    if (instrument.length === 0) return NextResponse.json({ error: "instrument required" }, { status: 400 });
    const hedgeMarket = typeof body.hedgeMarket === "string" ? body.hedgeMarket.trim().toUpperCase() : "";
    if (!/^[A-Z0-9]+-[A-Z0-9]+$/.test(hedgeMarket)) return NextResponse.json({ error: "hedgeMarket must look like BASE-QUOTE" }, { status: 400 });
    const target = num(body.target, "target");
    if (target <= 0) return NextResponse.json({ error: "target must be > 0" }, { status: 400 });
    const softTolerance = num(body.softTolerance, "softTolerance");
    const hardTolerance = num(body.hardTolerance, "hardTolerance");
    if (softTolerance <= 0 || hardTolerance <= 0 || hardTolerance <= softTolerance) {
      return NextResponse.json({ error: "require 0 < softTolerance < hardTolerance" }, { status: 400 });
    }
    const startingBalance = num(body.startingBalance, "startingBalance");
    if (startingBalance < 0) return NextResponse.json({ error: "startingBalance must be >= 0" }, { status: 400 });
    const startingPrice = num(body.startingPrice, "startingPrice");
    if (startingPrice <= 0) return NextResponse.json({ error: "startingPrice must be > 0" }, { status: 400 });
    const autoHedge = Boolean(body.autoHedge);
    const checkIntervalSecs = Math.max(1, Math.min(300, Math.floor(num(body.checkIntervalSecs, "checkIntervalSecs"))));
    const driftPerCycle = num(body.driftPerCycle, "driftPerCycle");

    const config: InvRiskConfig = {
      instrument,
      hedgeMarket,
      target,
      softTolerance,
      hardTolerance,
      startingBalance,
      startingPrice,
      autoHedge,
      checkIntervalSecs,
      driftPerCycle,
    };
    const state = startAgent(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
