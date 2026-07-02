import { NextResponse } from "next/server";
import { startTradingHistory } from "@/lib/store";
import type { TradingHistoryConfig } from "@/lib/tradinghistory-engine";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function num(v: unknown, name: string): number {
  const n = typeof v === "number" ? v : Number(v);
  if (!Number.isFinite(n)) throw new Error(`${name} must be a finite number`);
  return n;
}

function bool(v: unknown, def: boolean): boolean {
  if (v === undefined || v === null || v === "") return def;
  if (typeof v === "boolean") return v;
  if (typeof v === "string") return v === "true" || v === "1" || v === "on";
  return Boolean(v);
}

export async function POST(req: Request): Promise<Response> {
  const body = (await req.json()) as Record<string, unknown>;
  try {
    const marketRaw = body.market;
    const market = typeof marketRaw === "string" && marketRaw.trim().length > 0 ? marketRaw.trim() : undefined;
    const config: TradingHistoryConfig = {
      orders: bool(body.orders, true),
      settlements: bool(body.settlements, true),
      market,
      eventArrivalPerTick: num(body.eventArrivalPerTick, "eventArrivalPerTick"),
      startingPrice: num(body.startingPrice, "startingPrice"),
    };
    if (!config.orders && !config.settlements) {
      return NextResponse.json({ error: "orders and settlements both disabled — nothing to log" }, { status: 400 });
    }
    if (config.eventArrivalPerTick < 0 || config.eventArrivalPerTick > 10) {
      return NextResponse.json({ error: "eventArrivalPerTick must be in [0, 10]" }, { status: 400 });
    }
    if (config.startingPrice <= 0) {
      return NextResponse.json({ error: "startingPrice must be > 0" }, { status: 400 });
    }
    const state = startTradingHistory(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
