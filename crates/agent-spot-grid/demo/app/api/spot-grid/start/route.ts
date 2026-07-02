import { NextResponse } from "next/server";
import { startSpotGrid } from "@/lib/store";
import type { SpotGridConfig } from "@/lib/spotgrid-engine";

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
    const config: SpotGridConfig = {
      market: String(body.market ?? "CC-USDC"),
      midPrice: num(body.midPrice, "midPrice"),
      bidLevels: num(body.bidLevels, "bidLevels"),
      offerLevels: num(body.offerLevels, "offerLevels"),
      stepPct: num(body.stepPct, "stepPct"),
      qtyPerLevel: num(body.qtyPerLevel, "qtyPerLevel"),
      startingPrice: num(body.startingPrice, "startingPrice"),
    };
    if (!Number.isInteger(config.bidLevels) || config.bidLevels < 0) return NextResponse.json({ error: "bidLevels must be a non-negative integer" }, { status: 400 });
    if (!Number.isInteger(config.offerLevels) || config.offerLevels < 0) return NextResponse.json({ error: "offerLevels must be a non-negative integer" }, { status: 400 });
    if (config.bidLevels + config.offerLevels === 0) return NextResponse.json({ error: "at least one bid or offer level is required" }, { status: 400 });
    if (config.stepPct <= 0) return NextResponse.json({ error: "stepPct must be > 0" }, { status: 400 });
    if (config.midPrice <= 0) return NextResponse.json({ error: "midPrice must be > 0" }, { status: 400 });
    if (config.qtyPerLevel <= 0) return NextResponse.json({ error: "qtyPerLevel must be > 0" }, { status: 400 });
    if (config.startingPrice <= 0) return NextResponse.json({ error: "startingPrice must be > 0" }, { status: 400 });
    // Guard: with bidLevels * stepPct >= 100 the deepest bid goes to or below zero.
    if (config.stepPct * config.bidLevels >= 100) return NextResponse.json({ error: "stepPct × bidLevels must be < 100 (deepest bid must be positive)" }, { status: 400 });
    const state = startSpotGrid(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
