import { NextResponse } from "next/server";
import { startYieldRotation } from "@/lib/store";
import type { YieldRotationConfig } from "@/lib/yieldrotation-engine";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function num(v: unknown, name: string): number {
  const n = typeof v === "number" ? v : Number(v);
  if (!Number.isFinite(n)) throw new Error(`${name} must be a finite number`);
  return n;
}

function parseMarkets(v: unknown): string[] {
  if (Array.isArray(v)) return v.map((x) => String(x).trim()).filter(Boolean);
  if (typeof v === "string") return v.split(",").map((s) => s.trim()).filter(Boolean);
  return [];
}

export async function POST(req: Request): Promise<Response> {
  const body = (await req.json()) as Record<string, unknown>;
  try {
    const markets = parseMarkets(body.markets);
    if (markets.length === 0) {
      return NextResponse.json({ error: "markets must be a non-empty list" }, { status: 400 });
    }
    const config: YieldRotationConfig = {
      markets,
      wChange: num(body.wChange, "wChange"),
      wVolume: num(body.wVolume, "wVolume"),
      wSpread: num(body.wSpread, "wSpread"),
      pollSecs: num(body.pollSecs, "pollSecs"),
      startingPrice: num(body.startingPrice, "startingPrice"),
    };
    if (config.wChange < 0) return NextResponse.json({ error: "wChange must be >= 0" }, { status: 400 });
    if (config.wVolume < 0) return NextResponse.json({ error: "wVolume must be >= 0" }, { status: 400 });
    if (config.wSpread < 0) return NextResponse.json({ error: "wSpread must be >= 0" }, { status: 400 });
    if (config.pollSecs <= 0) return NextResponse.json({ error: "pollSecs must be > 0" }, { status: 400 });
    if (config.startingPrice <= 0) return NextResponse.json({ error: "startingPrice must be > 0" }, { status: 400 });
    const state = startYieldRotation(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
