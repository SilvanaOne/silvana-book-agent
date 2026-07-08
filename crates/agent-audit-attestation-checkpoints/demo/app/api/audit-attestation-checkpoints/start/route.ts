import { NextResponse } from "next/server";
import { startAgent } from "@/lib/store";
import type { AttestConfig } from "@/lib/attest-engine";

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
    const party = typeof body.party === "string" && body.party.trim().length > 0 ? body.party.trim() : "party::demo";
    const historyGrowthPerSec = num(body.historyGrowthPerSec, "historyGrowthPerSec");
    if (historyGrowthPerSec <= 0 || historyGrowthPerSec > 20) throw new Error("historyGrowthPerSec in (0, 20]");
    const intervalSecs = Math.max(0, Math.floor(num(body.intervalSecs, "intervalSecs")));
    const webhookFailureRate = num(body.webhookFailureRate, "webhookFailureRate");
    if (webhookFailureRate < 0 || webhookFailureRate > 1) throw new Error("webhookFailureRate in [0, 1]");

    const config: AttestConfig = { party, historyGrowthPerSec, intervalSecs, webhookFailureRate };
    const state = startAgent(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
