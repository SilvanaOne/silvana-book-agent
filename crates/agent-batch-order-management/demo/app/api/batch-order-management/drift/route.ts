import { NextResponse } from "next/server";
import { imitateDrift, setThresholdBps, updateWalk } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

// Demo tools: imitate drift on an asset, retune the drift threshold, or set the
// gentle price-walk vol. Each is optional; the body may carry any combination.
export async function POST(req: Request): Promise<Response> {
  const body = (await req.json().catch(() => ({}))) as Record<string, unknown>;

  if (body.thresholdBps !== undefined) {
    const n = Number(body.thresholdBps);
    if (!Number.isFinite(n) || n < 0) return NextResponse.json({ error: "thresholdBps invalid" }, { status: 400 });
    setThresholdBps(Math.round(n));
  }

  if (body.volPerTick !== undefined) {
    const n = Number(body.volPerTick);
    if (!Number.isFinite(n) || n < 0) return NextResponse.json({ error: "volPerTick invalid" }, { status: 400 });
    updateWalk({ volPerTick: n });
  }

  if (body.assetSymbol !== undefined || body.driftWeight !== undefined) {
    const assetSymbol = String(body.assetSymbol ?? "").trim();
    const driftWeight = Number(body.driftWeight);
    if (!assetSymbol) return NextResponse.json({ error: "assetSymbol required" }, { status: 400 });
    if (!Number.isFinite(driftWeight)) return NextResponse.json({ error: "driftWeight must be a finite number" }, { status: 400 });
    const res = imitateDrift(assetSymbol, driftWeight);
    if ("error" in res) return NextResponse.json({ error: res.error }, { status: 400 });
  }

  return NextResponse.json({ ok: true });
}
