import { NextResponse } from "next/server";
import { startCashBuffer } from "@/lib/store";
import type { CashBufferConfig } from "@/lib/cashbuffer-engine";

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
    const config: CashBufferConfig = {
      minCc: num(body.minCc, "minCc"),
      maxCc: num(body.maxCc, "maxCc"),
      sinkParty: String(body.sinkParty ?? "party-treasury").trim() || "party-treasury",
      checkIntervalSecs: num(body.checkIntervalSecs, "checkIntervalSecs"),
      incomeRate: num(body.incomeRate, "incomeRate"),
      startingBalance: num(body.startingBalance, "startingBalance"),
    };
    if (config.minCc <= 0) return NextResponse.json({ error: "minCc must be > 0" }, { status: 400 });
    if (config.maxCc <= config.minCc) return NextResponse.json({ error: "maxCc must be > minCc" }, { status: 400 });
    if (config.checkIntervalSecs <= 0) return NextResponse.json({ error: "checkIntervalSecs must be > 0" }, { status: 400 });
    if (config.incomeRate < 0) return NextResponse.json({ error: "incomeRate must be >= 0" }, { status: 400 });
    if (config.startingBalance < 0) return NextResponse.json({ error: "startingBalance must be >= 0" }, { status: 400 });
    const state = startCashBuffer(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
