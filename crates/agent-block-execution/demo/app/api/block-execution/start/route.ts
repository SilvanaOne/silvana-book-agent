import { NextResponse } from "next/server";
import { startBlockExecution } from "@/lib/store";
import type { BlockExecutionConfig, Side } from "@/lib/blockexecution-engine";

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
    const timeSlices = Math.floor(num(body.timeSlices, "timeSlices"));
    const config: BlockExecutionConfig = {
      market: String(body.market ?? "CC-USDC"),
      side: sideRaw as Side,
      total: num(body.total, "total"),
      price: num(body.price, "price"),
      timeSlices,
      durationSecs: num(body.durationSecs, "durationSecs"),
      visible: num(body.visible, "visible"),
      startingPrice: num(body.startingPrice, "startingPrice"),
    };
    if (config.total <= 0) return NextResponse.json({ error: "total must be > 0" }, { status: 400 });
    if (config.price <= 0) return NextResponse.json({ error: "price must be > 0" }, { status: 400 });
    if (config.timeSlices < 1) return NextResponse.json({ error: "timeSlices must be >= 1" }, { status: 400 });
    if (config.durationSecs <= 0) return NextResponse.json({ error: "durationSecs must be > 0" }, { status: 400 });
    if (config.visible <= 0) return NextResponse.json({ error: "visible must be > 0" }, { status: 400 });
    if (config.visible > config.total) return NextResponse.json({ error: "visible must be <= total" }, { status: 400 });
    if (config.startingPrice <= 0) return NextResponse.json({ error: "startingPrice must be > 0" }, { status: 400 });
    const state = startBlockExecution(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
