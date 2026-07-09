import { NextResponse } from "next/server";
import { startSpreadCapture } from "@/lib/store";
import type { SpreadCaptureConfig } from "@/lib/spreadcapture-engine";

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
    const levelsRaw = num(body.levels, "levels");
    if (!Number.isInteger(levelsRaw) || levelsRaw < 1) {
      return NextResponse.json({ error: "levels must be an integer >= 1" }, { status: 400 });
    }
    const config: SpreadCaptureConfig = {
      market: String(body.market ?? "CC-USDC"),
      levels: levelsRaw,
      innerSpreadBps: num(body.innerSpreadBps, "innerSpreadBps"),
      stepBps: num(body.stepBps, "stepBps"),
      quantityPerLevel: num(body.quantityPerLevel, "quantityPerLevel"),
      maxInventory: num(body.maxInventory, "maxInventory"),
      refreshSecs: num(body.refreshSecs, "refreshSecs"),
      startingPrice: num(body.startingPrice, "startingPrice"),
    };
    if (config.innerSpreadBps <= 0) return NextResponse.json({ error: "innerSpreadBps must be > 0" }, { status: 400 });
    if (config.stepBps < 0) return NextResponse.json({ error: "stepBps must be >= 0" }, { status: 400 });
    if (config.quantityPerLevel <= 0) return NextResponse.json({ error: "quantityPerLevel must be > 0" }, { status: 400 });
    if (config.maxInventory < 0) return NextResponse.json({ error: "maxInventory must be >= 0" }, { status: 400 });
    if (config.refreshSecs <= 0) return NextResponse.json({ error: "refreshSecs must be > 0" }, { status: 400 });
    if (config.startingPrice <= 0) return NextResponse.json({ error: "startingPrice must be > 0" }, { status: 400 });
    if (config.levels > 50) return NextResponse.json({ error: "levels must be <= 50" }, { status: 400 });
    const state = startSpreadCapture(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
