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
    const config: TrendAnalysisConfig = {
      market: String(body.market ?? "CC-USDC"),
      window: intGe(body.window, "window", 2),
      rsiPeriod: intGe(body.rsiPeriod, "rsiPeriod", 2),
      bollingerK: num(body.bollingerK, "bollingerK"),
      macdFast: intGe(body.macdFast, "macdFast", 2),
      macdSlow: intGe(body.macdSlow, "macdSlow", 2),
      macdSignal: intGe(body.macdSignal, "macdSignal", 1),
      startingPrice: num(body.startingPrice, "startingPrice"),
    };
    if (config.bollingerK <= 0) {
      return NextResponse.json({ error: "bollingerK must be > 0" }, { status: 400 });
    }
    if (config.macdFast >= config.macdSlow) {
      return NextResponse.json({ error: "macdFast must be < macdSlow" }, { status: 400 });
    }
    if (config.startingPrice <= 0) {
      return NextResponse.json({ error: "startingPrice must be > 0" }, { status: 400 });
    }
    const state = startTrendAnalysis(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
