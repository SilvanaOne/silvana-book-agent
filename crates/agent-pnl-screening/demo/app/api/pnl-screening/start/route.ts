import { NextResponse } from "next/server";
import { startPnlScreening } from "@/lib/store";
import type { PnlScreeningConfig } from "@/lib/pnlscreening-engine";

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
    const config: PnlScreeningConfig = {
      market: String(body.market ?? "CC-USDC"),
      snapshotIntervalSecs: num(body.snapshotIntervalSecs, "snapshotIntervalSecs"),
      tradeArrivalPerTick: num(body.tradeArrivalPerTick, "tradeArrivalPerTick"),
      avgTradeQty: num(body.avgTradeQty, "avgTradeQty"),
      startingPrice: num(body.startingPrice, "startingPrice"),
      startingPosition: num(body.startingPosition, "startingPosition"),
      startingCostBasis: num(body.startingCostBasis, "startingCostBasis"),
    };
    if (config.snapshotIntervalSecs <= 0) return NextResponse.json({ error: "snapshotIntervalSecs must be > 0" }, { status: 400 });
    if (config.tradeArrivalPerTick < 0 || config.tradeArrivalPerTick > 5) return NextResponse.json({ error: "tradeArrivalPerTick must be in [0, 5]" }, { status: 400 });
    if (config.avgTradeQty <= 0) return NextResponse.json({ error: "avgTradeQty must be > 0" }, { status: 400 });
    if (config.startingPrice <= 0) return NextResponse.json({ error: "startingPrice must be > 0" }, { status: 400 });
    if (config.startingPosition < 0) return NextResponse.json({ error: "startingPosition must be >= 0" }, { status: 400 });
    if (config.startingCostBasis < 0) return NextResponse.json({ error: "startingCostBasis must be >= 0" }, { status: 400 });
    const state = startPnlScreening(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
