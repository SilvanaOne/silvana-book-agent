import { NextResponse } from "next/server";
import { startFailureRecovery } from "@/lib/store";
import type { FailureRecoveryConfig } from "@/lib/failurerecovery-engine";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function num(v: unknown, name: string): number {
  const n = typeof v === "number" ? v : Number(v);
  if (!Number.isFinite(n)) throw new Error(`${name} must be a finite number`);
  return n;
}

function bool(v: unknown): boolean {
  if (typeof v === "boolean") return v;
  if (typeof v === "string") return v === "true" || v === "1" || v === "yes";
  return Boolean(v);
}

export async function POST(req: Request): Promise<Response> {
  const body = (await req.json()) as Record<string, unknown>;
  try {
    const config: FailureRecoveryConfig = {
      maxPendingAgeSecs: num(body.maxPendingAgeSecs, "maxPendingAgeSecs"),
      checkIntervalSecs: num(body.checkIntervalSecs, "checkIntervalSecs"),
      cancelRelatedOrders: bool(body.cancelRelatedOrders),
      dryRun: bool(body.dryRun),
      proposalArrivalPerTick: num(body.proposalArrivalPerTick, "proposalArrivalPerTick"),
      pendingStuckProbability: num(body.pendingStuckProbability, "pendingStuckProbability"),
      failureProbability: num(body.failureProbability, "failureProbability"),
      startingPrice: num(body.startingPrice, "startingPrice"),
    };
    if (config.maxPendingAgeSecs <= 0) return NextResponse.json({ error: "maxPendingAgeSecs must be > 0" }, { status: 400 });
    if (config.checkIntervalSecs <= 0) return NextResponse.json({ error: "checkIntervalSecs must be > 0" }, { status: 400 });
    if (config.proposalArrivalPerTick < 0 || config.proposalArrivalPerTick > 10) return NextResponse.json({ error: "proposalArrivalPerTick must be in [0, 10]" }, { status: 400 });
    if (config.pendingStuckProbability < 0 || config.pendingStuckProbability > 1) return NextResponse.json({ error: "pendingStuckProbability must be in [0, 1]" }, { status: 400 });
    if (config.failureProbability < 0 || config.failureProbability > 1) return NextResponse.json({ error: "failureProbability must be in [0, 1]" }, { status: 400 });
    if (config.startingPrice <= 0) return NextResponse.json({ error: "startingPrice must be > 0" }, { status: 400 });
    const state = startFailureRecovery(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
