import { NextResponse } from "next/server";
import { startStateMonitor } from "@/lib/store";
import type { StateMonitorConfig } from "@/lib/statemonitor-engine";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function num(v: unknown, name: string): number {
  const n = typeof v === "number" ? v : Number(v);
  if (!Number.isFinite(n)) throw new Error(`${name} must be a finite number`);
  return n;
}

function bool(v: unknown, dflt: boolean): boolean {
  if (typeof v === "boolean") return v;
  if (typeof v === "string") {
    const s = v.toLowerCase();
    if (s === "true" || s === "1" || s === "yes") return true;
    if (s === "false" || s === "0" || s === "no") return false;
  }
  return dflt;
}

export async function POST(req: Request): Promise<Response> {
  const body = (await req.json()) as Record<string, unknown>;
  try {
    const config: StateMonitorConfig = {
      market: String(body.market ?? ""),
      includeOrders: bool(body.includeOrders, true),
      includeSettlements: bool(body.includeSettlements, true),
      orderArrivalPerTick: num(body.orderArrivalPerTick, "orderArrivalPerTick"),
      settlementArrivalPerTick: num(body.settlementArrivalPerTick, "settlementArrivalPerTick"),
      startingPrice: num(body.startingPrice, "startingPrice"),
    };
    if (!config.includeOrders && !config.includeSettlements) {
      return NextResponse.json({ error: "at least one of includeOrders / includeSettlements must be true" }, { status: 400 });
    }
    if (config.orderArrivalPerTick < 0 || config.orderArrivalPerTick > 10) {
      return NextResponse.json({ error: "orderArrivalPerTick must be in [0, 10]" }, { status: 400 });
    }
    if (config.settlementArrivalPerTick < 0 || config.settlementArrivalPerTick > 10) {
      return NextResponse.json({ error: "settlementArrivalPerTick must be in [0, 10]" }, { status: 400 });
    }
    if (config.startingPrice <= 0) {
      return NextResponse.json({ error: "startingPrice must be > 0" }, { status: 400 });
    }
    const state = startStateMonitor(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
