import { NextResponse } from "next/server";
import { startPairsTrading } from "@/lib/store";
import type { PairsTradingConfig } from "@/lib/pairstrading-engine";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function num(v: unknown, name: string): number {
  const n = typeof v === "number" ? v : Number(v);
  if (!Number.isFinite(n)) throw new Error(`${name} must be a finite number`);
  return n;
}

function intNum(v: unknown, name: string): number {
  const n = num(v, name);
  if (!Number.isInteger(n)) throw new Error(`${name} must be an integer`);
  return n;
}

export async function POST(req: Request): Promise<Response> {
  const body = (await req.json()) as Record<string, unknown>;
  try {
    const window = intNum(body.window, "window");
    const warmupRaw = body.warmupSamples === undefined || body.warmupSamples === null || body.warmupSamples === "" ? window : body.warmupSamples;
    const config: PairsTradingConfig = {
      marketA: String(body.marketA ?? "CC-USDC"),
      marketB: String(body.marketB ?? "CC-USDT"),
      window,
      entryZ: num(body.entryZ, "entryZ"),
      exitZ: num(body.exitZ, "exitZ"),
      quantityA: num(body.quantityA, "quantityA"),
      quantityB: num(body.quantityB, "quantityB"),
      warmupSamples: intNum(warmupRaw, "warmupSamples"),
      startingPriceA: num(body.startingPriceA, "startingPriceA"),
      startingPriceB: num(body.startingPriceB, "startingPriceB"),
    };
    if (!config.marketA) return NextResponse.json({ error: "marketA is required" }, { status: 400 });
    if (!config.marketB) return NextResponse.json({ error: "marketB is required" }, { status: 400 });
    if (config.marketA === config.marketB) return NextResponse.json({ error: "market A and market B must differ" }, { status: 400 });
    if (config.window < 3) return NextResponse.json({ error: "window must be >= 3" }, { status: 400 });
    if (config.entryZ <= 0) return NextResponse.json({ error: "entryZ must be > 0" }, { status: 400 });
    if (config.exitZ < 0) return NextResponse.json({ error: "exitZ must be >= 0" }, { status: 400 });
    if (config.exitZ >= config.entryZ) return NextResponse.json({ error: "exitZ must be < entryZ" }, { status: 400 });
    if (config.quantityA <= 0) return NextResponse.json({ error: "quantityA must be > 0" }, { status: 400 });
    if (config.quantityB <= 0) return NextResponse.json({ error: "quantityB must be > 0" }, { status: 400 });
    if (config.warmupSamples < 3) return NextResponse.json({ error: "warmupSamples must be >= 3" }, { status: 400 });
    if (config.warmupSamples > config.window) return NextResponse.json({ error: "warmupSamples must be <= window" }, { status: 400 });
    if (config.startingPriceA <= 0) return NextResponse.json({ error: "startingPriceA must be > 0" }, { status: 400 });
    if (config.startingPriceB <= 0) return NextResponse.json({ error: "startingPriceB must be > 0" }, { status: 400 });
    const state = startPairsTrading(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
