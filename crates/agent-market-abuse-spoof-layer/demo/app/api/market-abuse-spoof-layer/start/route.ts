import { NextResponse } from "next/server";
import { startMarketAbuse } from "@/lib/store";
import type { MarketAbuseConfig } from "@/lib/marketabuse-engine";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function num(v: unknown, name: string): number {
  const n = typeof v === "number" ? v : Number(v);
  if (!Number.isFinite(n)) throw new Error(`${name} must be a finite number`);
  return n;
}

function bool(v: unknown, def: boolean): boolean {
  if (typeof v === "boolean") return v;
  if (typeof v === "string") {
    if (v === "true") return true;
    if (v === "false") return false;
  }
  return def;
}

export async function POST(req: Request): Promise<Response> {
  const body = (await req.json()) as Record<string, unknown>;
  try {
    const config: MarketAbuseConfig = {
      market: String(body.market ?? "CC-USDC"),
      spoofBurst: num(body.spoofBurst, "spoofBurst"),
      spoofBurstWindowSecs: num(body.spoofBurstWindowSecs, "spoofBurstWindowSecs"),
      spoofWindowSecs: num(body.spoofWindowSecs, "spoofWindowSecs"),
      layerMinOrders: num(body.layerMinOrders, "layerMinOrders"),
      layerPriceBandPct: num(body.layerPriceBandPct, "layerPriceBandPct"),
      orderArrivalPerTick: num(body.orderArrivalPerTick, "orderArrivalPerTick"),
      cancelRatePerTick: num(body.cancelRatePerTick, "cancelRatePerTick"),
      spoofScenarioEnabled: bool(body.spoofScenarioEnabled, true),
      startingPrice: num(body.startingPrice, "startingPrice"),
    };
    if (config.spoofBurst <= 0) return NextResponse.json({ error: "spoofBurst must be > 0" }, { status: 400 });
    if (config.spoofBurstWindowSecs <= 0) return NextResponse.json({ error: "spoofBurstWindowSecs must be > 0" }, { status: 400 });
    if (config.spoofWindowSecs <= 0) return NextResponse.json({ error: "spoofWindowSecs must be > 0" }, { status: 400 });
    if (config.layerMinOrders <= 0) return NextResponse.json({ error: "layerMinOrders must be > 0" }, { status: 400 });
    if (config.layerPriceBandPct <= 0) return NextResponse.json({ error: "layerPriceBandPct must be > 0" }, { status: 400 });
    if (config.orderArrivalPerTick < 0 || config.orderArrivalPerTick > 10) return NextResponse.json({ error: "orderArrivalPerTick must be in [0, 10]" }, { status: 400 });
    if (config.cancelRatePerTick < 0 || config.cancelRatePerTick > 1) return NextResponse.json({ error: "cancelRatePerTick must be in [0, 1]" }, { status: 400 });
    if (config.startingPrice <= 0) return NextResponse.json({ error: "startingPrice must be > 0" }, { status: 400 });
    const state = startMarketAbuse(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
