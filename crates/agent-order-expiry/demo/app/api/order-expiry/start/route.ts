import { NextResponse } from "next/server";
import { startOrderExpiry } from "@/lib/store";
import type { OrderExpiryConfig } from "@/lib/orderexpiry-engine";

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
    const config: OrderExpiryConfig = {
      market: String(body.market ?? "CC-USDC"),
      maxAgeSecs: num(body.maxAgeSecs, "maxAgeSecs"),
      checkIntervalSecs: num(body.checkIntervalSecs, "checkIntervalSecs"),
      orderArrivalPerTick: num(body.orderArrivalPerTick, "orderArrivalPerTick"),
      dryRun: Boolean(body.dryRun),
      startingPrice: num(body.startingPrice, "startingPrice"),
    };
    if (config.maxAgeSecs <= 0) return NextResponse.json({ error: "maxAgeSecs must be > 0" }, { status: 400 });
    if (config.checkIntervalSecs <= 0) return NextResponse.json({ error: "checkIntervalSecs must be > 0" }, { status: 400 });
    if (config.orderArrivalPerTick < 0 || config.orderArrivalPerTick > 10) {
      return NextResponse.json({ error: "orderArrivalPerTick must be in [0, 10]" }, { status: 400 });
    }
    if (config.startingPrice <= 0) return NextResponse.json({ error: "startingPrice must be > 0" }, { status: 400 });
    const state = startOrderExpiry(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
