import { NextResponse } from "next/server";
import { startKillswitch } from "@/lib/store";
import type { KillswitchConfig } from "@/lib/killswitch-engine";

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
    const config: KillswitchConfig = {
      maxOpenOrders: num(body.maxOpenOrders, "maxOpenOrders"),
      maxFailedSettlements: num(body.maxFailedSettlements, "maxFailedSettlements"),
      checkIntervalSecs: num(body.checkIntervalSecs, "checkIntervalSecs"),
      orderGrowthPerTick: num(body.orderGrowthPerTick, "orderGrowthPerTick"),
      failureRatePerTick: num(body.failureRatePerTick, "failureRatePerTick"),
      startingOpenOrders: num(body.startingOpenOrders, "startingOpenOrders"),
      startingFailed: num(body.startingFailed, "startingFailed"),
    };
    if (config.maxOpenOrders <= 0) return NextResponse.json({ error: "maxOpenOrders must be > 0" }, { status: 400 });
    if (config.maxFailedSettlements < 0) return NextResponse.json({ error: "maxFailedSettlements must be >= 0" }, { status: 400 });
    if (config.checkIntervalSecs <= 0) return NextResponse.json({ error: "checkIntervalSecs must be > 0" }, { status: 400 });
    if (config.orderGrowthPerTick < 0) return NextResponse.json({ error: "orderGrowthPerTick must be >= 0" }, { status: 400 });
    if (config.failureRatePerTick < 0 || config.failureRatePerTick > 1) return NextResponse.json({ error: "failureRatePerTick must be in [0,1]" }, { status: 400 });
    if (config.startingOpenOrders < 0) return NextResponse.json({ error: "startingOpenOrders must be >= 0" }, { status: 400 });
    if (config.startingFailed < 0) return NextResponse.json({ error: "startingFailed must be >= 0" }, { status: 400 });
    const state = startKillswitch(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
