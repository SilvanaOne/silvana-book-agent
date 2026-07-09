import { NextResponse } from "next/server";
import { startTrendAnalysis } from "@/lib/store";
import type { TrendAnalysisConfig } from "@/lib/trendanalysis-engine";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function num(v: unknown, name: string): number {
  const n = typeof v === "number" ? v : Number(v);
  if (!Number.isFinite(n)) throw new Error(`${name} must be a finite number`);
  return n;
}

function intGe(v: unknown, name: string, min: number): number {
  const n = num(v, name);
  if (!Number.isInteger(n)) throw new Error(`${name} must be an integer`);
  if (n < min) throw new Error(`${name} must be >= ${min}`);
  return n;
}

export async function POST(req: Request): Promise<Response> {
  const body = (await req.json()) as Record<string, unknown>;
  try {
    const market = String(body.market ?? "CC-USDC");
    if (!market.trim()) throw new Error("market must be non-empty");

    const short = intGe(body.short, "short", 2);
    const mid = intGe(body.mid, "mid", 2);
    const long = intGe(body.long, "long", 2);
    if (!(short < mid)) throw new Error("short must be < mid");
    if (!(mid < long)) throw new Error("mid must be < long");

    const startingPrice = num(body.startingPrice, "startingPrice");
    if (startingPrice <= 0) throw new Error("startingPrice must be > 0");

    const config: TrendAnalysisConfig = {
      market,
      short,
      mid,
      long,
      startingPrice,
    };
    const state = startTrendAnalysis(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
