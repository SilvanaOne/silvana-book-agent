import { NextResponse } from "next/server";
import { startInventoryMgmt } from "@/lib/store";
import type { InventoryMgmtConfig } from "@/lib/inventorymgmt-engine";

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
    const config: InventoryMgmtConfig = {
      market: String(body.market ?? "CC-USDC"),
      instrument: String(body.instrument ?? "Amulet"),
      target: num(body.target, "target"),
      tolerance: num(body.tolerance, "tolerance"),
      chunkSize: num(body.chunkSize, "chunkSize"),
      checkIntervalSecs: num(body.checkIntervalSecs, "checkIntervalSecs"),
      priceOffsetPct: num(body.priceOffsetPct, "priceOffsetPct"),
      startingBalance: num(body.startingBalance, "startingBalance"),
      startingPrice: num(body.startingPrice, "startingPrice"),
    };
    if (config.target <= 0) return NextResponse.json({ error: "target must be > 0" }, { status: 400 });
    if (config.tolerance < 0) return NextResponse.json({ error: "tolerance must be >= 0" }, { status: 400 });
    if (config.chunkSize <= 0) return NextResponse.json({ error: "chunkSize must be > 0" }, { status: 400 });
    if (config.checkIntervalSecs <= 0) return NextResponse.json({ error: "checkIntervalSecs must be > 0" }, { status: 400 });
    if (config.startingBalance < 0) return NextResponse.json({ error: "startingBalance must be >= 0" }, { status: 400 });
    if (config.startingPrice <= 0) return NextResponse.json({ error: "startingPrice must be > 0" }, { status: 400 });
    if (!config.instrument.trim()) return NextResponse.json({ error: "instrument is required" }, { status: 400 });
    const state = startInventoryMgmt(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
