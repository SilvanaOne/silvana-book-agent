import { NextResponse } from "next/server";
import { startCircuitBreaker } from "@/lib/store";
import type { CircuitBreakerConfig } from "@/lib/circuitbreaker-engine";

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
    const config: CircuitBreakerConfig = {
      market: String(body.market ?? "CC-USDC"),
      maxDeviationPct: num(body.maxDeviationPct, "maxDeviationPct"),
      windowSecs: num(body.windowSecs, "windowSecs"),
      pauseSecs: num(body.pauseSecs, "pauseSecs"),
      startingPrice: num(body.startingPrice, "startingPrice"),
      startingOpenOrders: num(body.startingOpenOrders, "startingOpenOrders"),
    };
    if (config.maxDeviationPct <= 0) return NextResponse.json({ error: "maxDeviationPct must be > 0" }, { status: 400 });
    if (config.windowSecs <= 0) return NextResponse.json({ error: "windowSecs must be > 0" }, { status: 400 });
    if (config.pauseSecs <= 0) return NextResponse.json({ error: "pauseSecs must be > 0" }, { status: 400 });
    if (config.startingPrice <= 0) return NextResponse.json({ error: "startingPrice must be > 0" }, { status: 400 });
    if (config.startingOpenOrders < 0) return NextResponse.json({ error: "startingOpenOrders must be >= 0" }, { status: 400 });
    const state = startCircuitBreaker(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
