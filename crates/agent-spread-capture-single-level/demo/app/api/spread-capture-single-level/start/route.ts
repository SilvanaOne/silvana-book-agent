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
    const config: SpreadCaptureConfig = {
      market: String(body.market ?? "CC-USDC"),
      spreadBps: num(body.spreadBps, "spreadBps"),
      quantity: num(body.quantity, "quantity"),
      maxInventory: num(body.maxInventory, "maxInventory"),
      refreshSecs: num(body.refreshSecs, "refreshSecs"),
      startingPrice: num(body.startingPrice, "startingPrice"),
    };
    if (config.spreadBps <= 0) return NextResponse.json({ error: "spreadBps must be > 0" }, { status: 400 });
    if (config.quantity <= 0) return NextResponse.json({ error: "quantity must be > 0" }, { status: 400 });
    if (config.maxInventory < 0) return NextResponse.json({ error: "maxInventory must be >= 0" }, { status: 400 });
    if (config.refreshSecs <= 0) return NextResponse.json({ error: "refreshSecs must be > 0" }, { status: 400 });
    if (config.startingPrice <= 0) return NextResponse.json({ error: "startingPrice must be > 0" }, { status: 400 });
    const state = startSpreadCapture(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
