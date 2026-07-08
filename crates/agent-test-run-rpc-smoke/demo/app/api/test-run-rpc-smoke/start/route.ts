import { NextResponse } from "next/server";
import { startAgent } from "@/lib/store";
import type { TestRunConfig } from "@/lib/testrun-engine";

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
    const partyId = typeof body.partyId === "string" ? body.partyId.trim() : "";
    if (partyId.length === 0) return NextResponse.json({ error: "partyId required" }, { status: 400 });
    const endpoint = typeof body.endpoint === "string" ? body.endpoint.trim() : "";
    if (endpoint.length === 0) return NextResponse.json({ error: "endpoint required" }, { status: 400 });
    const market = typeof body.market === "string" ? body.market.trim().toUpperCase() : "";
    const failureRate = num(body.failureRate, "failureRate");
    if (failureRate < 0 || failureRate > 1) return NextResponse.json({ error: "failureRate must be in [0, 1]" }, { status: 400 });
    const latencyMultiplier = num(body.latencyMultiplier, "latencyMultiplier");
    if (latencyMultiplier <= 0 || latencyMultiplier > 10) return NextResponse.json({ error: "latencyMultiplier must be in (0, 10]" }, { status: 400 });
    const runIntervalSecs = Math.max(0, Math.min(3600, Math.floor(num(body.runIntervalSecs, "runIntervalSecs"))));

    const config: TestRunConfig = { partyId, endpoint, market, failureRate, latencyMultiplier, runIntervalSecs };
    const state = startAgent(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
