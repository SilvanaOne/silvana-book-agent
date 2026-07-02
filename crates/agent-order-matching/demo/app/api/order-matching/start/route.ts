import { NextResponse } from "next/server";
import { startOrderMatching } from "@/lib/store";
import type { OrderMatchingConfig } from "@/lib/ordermatching-engine";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function num(v: unknown, name: string): number {
  const n = typeof v === "number" ? v : Number(v);
  if (!Number.isFinite(n)) throw new Error(`${name} must be a finite number`);
  return n;
}

function optNum(v: unknown, name: string): number | null {
  if (v === undefined || v === null || v === "") return null;
  const s = typeof v === "string" ? v.trim() : v;
  if (s === "") return null;
  const n = typeof s === "number" ? s : Number(s);
  if (!Number.isFinite(n)) throw new Error(`${name} must be a finite number or empty`);
  return n;
}

export async function POST(req: Request): Promise<Response> {
  const body = (await req.json()) as Record<string, unknown>;
  try {
    const buyTrigger = optNum(body.buyTrigger, "buyTrigger");
    const sellTrigger = optNum(body.sellTrigger, "sellTrigger");
    const config: OrderMatchingConfig = {
      market: String(body.market ?? "CC-USDC"),
      buyTrigger,
      sellTrigger,
      quantity: num(body.quantity, "quantity"),
      bookSpreadBps: num(body.bookSpreadBps, "bookSpreadBps"),
      startingPrice: num(body.startingPrice, "startingPrice"),
    };
    if (config.buyTrigger === null && config.sellTrigger === null) {
      return NextResponse.json(
        { error: "provide at least one of buyTrigger or sellTrigger" },
        { status: 400 },
      );
    }
    if (config.buyTrigger !== null && config.buyTrigger <= 0) {
      return NextResponse.json({ error: "buyTrigger must be > 0" }, { status: 400 });
    }
    if (config.sellTrigger !== null && config.sellTrigger <= 0) {
      return NextResponse.json({ error: "sellTrigger must be > 0" }, { status: 400 });
    }
    if (config.quantity <= 0) return NextResponse.json({ error: "quantity must be > 0" }, { status: 400 });
    if (config.startingPrice <= 0) return NextResponse.json({ error: "startingPrice must be > 0" }, { status: 400 });
    if (config.bookSpreadBps < 0) return NextResponse.json({ error: "bookSpreadBps must be >= 0" }, { status: 400 });
    const state = startOrderMatching(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
