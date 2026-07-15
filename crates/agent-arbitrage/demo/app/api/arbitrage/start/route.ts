import { NextResponse } from "next/server";
import { startArbitrage } from "@/lib/store";
import { CHART_PAIR_KEYS, type ChartPairKey } from "@/lib/demo-data";
import type { ArbitrageConfig } from "@/lib/arbitrage-engine";

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
    const focusPair = String(body.focusPair ?? "CC/USDC");
    if (!CHART_PAIR_KEYS.includes(focusPair as ChartPairKey)) {
      return NextResponse.json({ error: `unknown pair ${focusPair}` }, { status: 400 });
    }
    const config: ArbitrageConfig = {
      focusPair: focusPair as ChartPairKey,
      minSpreadBps: num(body.minSpreadBps, "minSpreadBps"),
      tradeSizeUsd: num(body.tradeSizeUsd, "tradeSizeUsd"),
      scanIntervalSecs: num(body.scanIntervalSecs, "scanIntervalSecs"),
    };
    if (config.minSpreadBps <= 0) return NextResponse.json({ error: "minSpreadBps must be > 0" }, { status: 400 });
    if (config.tradeSizeUsd <= 0) return NextResponse.json({ error: "tradeSizeUsd must be > 0" }, { status: 400 });
    if (config.scanIntervalSecs <= 0)
      return NextResponse.json({ error: "scanIntervalSecs must be > 0" }, { status: 400 });
    const state = startArbitrage(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
