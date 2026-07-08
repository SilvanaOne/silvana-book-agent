import { NextResponse } from "next/server";
import { startHumanApproval } from "@/lib/store";
import type { HumanApprovalConfig } from "@/lib/humanapproval-engine";

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
    const reviewer = String(body.reviewerName ?? "operator").trim();
    if (reviewer.length === 0)
      return NextResponse.json({ error: "reviewerName must not be empty" }, { status: 400 });

    const config: HumanApprovalConfig = {
      market: String(body.market ?? "CC-USDC"),
      orderArrivalPerTick: num(body.orderArrivalPerTick, "orderArrivalPerTick"),
      autoApprovalEnabled: Boolean(body.autoApprovalEnabled ?? false),
      autoApprovalThreshold: num(body.autoApprovalThreshold, "autoApprovalThreshold"),
      reviewerName: reviewer,
      startingPrice: num(body.startingPrice, "startingPrice"),
    };
    if (config.orderArrivalPerTick < 0 || config.orderArrivalPerTick > 10)
      return NextResponse.json({ error: "orderArrivalPerTick must be in [0, 10]" }, { status: 400 });
    if (config.autoApprovalThreshold < 0)
      return NextResponse.json({ error: "autoApprovalThreshold must be >= 0" }, { status: 400 });
    if (config.startingPrice <= 0)
      return NextResponse.json({ error: "startingPrice must be > 0" }, { status: 400 });

    const state = startHumanApproval(config);
    return NextResponse.json({ state });
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
