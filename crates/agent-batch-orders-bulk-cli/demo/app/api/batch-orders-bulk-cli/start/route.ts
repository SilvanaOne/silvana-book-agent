import { NextResponse } from "next/server";
import { startBatchOrders } from "@/lib/store";
import { parseOrdersJsonl, type BatchOrdersConfig } from "@/lib/batchorders-engine";

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
    const jsonl = String(body.ordersJsonl ?? "").trim();
    if (jsonl === "") {
      return NextResponse.json({ error: "ordersJsonl must not be empty" }, { status: 400 });
    }
    let orders;
    try {
      orders = parseOrdersJsonl(jsonl);
    } catch (e) {
      return NextResponse.json({ error: (e as Error).message }, { status: 400 });
    }
    if (orders.length === 0) {
      return NextResponse.json({ error: "no orders parsed from ordersJsonl" }, { status: 400 });
    }
    const submitRatePerTick = num(body.submitRatePerTick, "submitRatePerTick");
    const failureRatePerTick = num(body.failureRatePerTick, "failureRatePerTick");
    const startingPrice = num(body.startingPrice, "startingPrice");
    if (submitRatePerTick <= 0) {
      return NextResponse.json({ error: "submitRatePerTick must be > 0" }, { status: 400 });
    }
    if (failureRatePerTick < 0 || failureRatePerTick > 1) {
      return NextResponse.json({ error: "failureRatePerTick must be in [0, 1]" }, { status: 400 });
    }
    if (startingPrice <= 0) {
      return NextResponse.json({ error: "startingPrice must be > 0" }, { status: 400 });
    }
    const config: BatchOrdersConfig = {
      orders,
      submitRatePerTick: Math.max(1, Math.floor(submitRatePerTick)),
      abortOnError: Boolean(body.abortOnError),
      failureRatePerTick,
      startingPrice,
    };
    const state = startBatchOrders(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
