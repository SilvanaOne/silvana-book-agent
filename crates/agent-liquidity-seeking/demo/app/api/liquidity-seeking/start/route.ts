import { NextResponse } from "next/server";
import { startLiquiditySeeking } from "@/lib/store";
import type { LiquiditySeekingConfig, Side } from "@/lib/liquidityseeking-engine";

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
    const sideRaw = String(body.side ?? "buy").toLowerCase();
    if (sideRaw !== "buy" && sideRaw !== "sell") {
      return NextResponse.json({ error: "side must be 'buy' or 'sell'" }, { status: 400 });
    }
    const side = sideRaw as Side;

    const maxRuntimeSecsRaw = body.maxRuntimeSecs;
    const maxRuntimeSecs =
      maxRuntimeSecsRaw === undefined || maxRuntimeSecsRaw === null || maxRuntimeSecsRaw === ""
        ? undefined
        : num(maxRuntimeSecsRaw, "maxRuntimeSecs");

    const config: LiquiditySeekingConfig = {
      market: String(body.market ?? "CC-USDC"),
      side,
      total: num(body.total, "total"),
      maxSlippageBps: num(body.maxSlippageBps, "maxSlippageBps"),
      maxChunk: num(body.maxChunk, "maxChunk"),
      depth: Math.floor(num(body.depth, "depth")),
      maxRuntimeSecs,
      startingPrice: num(body.startingPrice, "startingPrice"),
      spreadBps: num(body.spreadBps ?? 10, "spreadBps"),
      depthFalloffPct: num(body.depthFalloffPct ?? 0.5, "depthFalloffPct"),
    };

    if (config.total <= 0) return NextResponse.json({ error: "total must be > 0" }, { status: 400 });
    if (config.maxSlippageBps <= 0) return NextResponse.json({ error: "maxSlippageBps must be > 0" }, { status: 400 });
    if (config.maxChunk <= 0) return NextResponse.json({ error: "maxChunk must be > 0" }, { status: 400 });
    if (config.depth < 1) return NextResponse.json({ error: "depth must be >= 1" }, { status: 400 });
    if (config.startingPrice <= 0) return NextResponse.json({ error: "startingPrice must be > 0" }, { status: 400 });
    if (config.spreadBps < 0) return NextResponse.json({ error: "spreadBps must be >= 0" }, { status: 400 });
    if (config.depthFalloffPct < 0) return NextResponse.json({ error: "depthFalloffPct must be >= 0" }, { status: 400 });
    if (typeof config.maxRuntimeSecs === "number" && config.maxRuntimeSecs <= 0) {
      return NextResponse.json({ error: "maxRuntimeSecs must be > 0 when provided" }, { status: 400 });
    }

    const state = startLiquiditySeeking(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
