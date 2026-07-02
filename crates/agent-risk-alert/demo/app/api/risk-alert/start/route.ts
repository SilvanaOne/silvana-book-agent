import { NextResponse } from "next/server";
import { startRiskAlert } from "@/lib/store";
import type { RiskAlertConfig } from "@/lib/riskalert-engine";

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
    const config: RiskAlertConfig = {
      maxOpenOrders: num(body.maxOpenOrders, "maxOpenOrders"),
      maxFailedSettlements: num(body.maxFailedSettlements, "maxFailedSettlements"),
      maxOpenNotional: num(body.maxOpenNotional, "maxOpenNotional"),
      checkIntervalSecs: num(body.checkIntervalSecs, "checkIntervalSecs"),
      orderGrowthPerTick: num(body.orderGrowthPerTick, "orderGrowthPerTick"),
      notionalGrowthPerTick: num(body.notionalGrowthPerTick, "notionalGrowthPerTick"),
      failureRatePerTick: num(body.failureRatePerTick, "failureRatePerTick"),
      startingPrice: num(body.startingPrice, "startingPrice"),
    };
    if (config.maxOpenOrders <= 0) return NextResponse.json({ error: "maxOpenOrders must be > 0" }, { status: 400 });
    if (config.maxFailedSettlements <= 0) return NextResponse.json({ error: "maxFailedSettlements must be > 0" }, { status: 400 });
    if (config.maxOpenNotional <= 0) return NextResponse.json({ error: "maxOpenNotional must be > 0" }, { status: 400 });
    if (config.checkIntervalSecs <= 0) return NextResponse.json({ error: "checkIntervalSecs must be > 0" }, { status: 400 });
    if (config.orderGrowthPerTick < 0) return NextResponse.json({ error: "orderGrowthPerTick must be >= 0" }, { status: 400 });
    if (config.notionalGrowthPerTick < 0) return NextResponse.json({ error: "notionalGrowthPerTick must be >= 0" }, { status: 400 });
    if (config.failureRatePerTick < 0 || config.failureRatePerTick > 1) return NextResponse.json({ error: "failureRatePerTick must be in [0,1]" }, { status: 400 });
    if (config.startingPrice <= 0) return NextResponse.json({ error: "startingPrice must be > 0" }, { status: 400 });
    const state = startRiskAlert(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
