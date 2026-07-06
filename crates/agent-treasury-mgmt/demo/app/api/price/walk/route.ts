import { NextResponse } from "next/server";
import { updateWalk } from "@/lib/store";
export const runtime = "nodejs";
export const dynamic = "force-dynamic";
export async function POST(req: Request): Promise<Response> {
  const body = (await req.json()) as Record<string, unknown>;
  const patch: { driftPerTick?: number; volPerTick?: number } = {};
  if (body.driftPerTick !== undefined) { const n = Number(body.driftPerTick); if (!Number.isFinite(n)) return NextResponse.json({ error: "driftPerTick invalid" }, { status: 400 }); patch.driftPerTick = n; }
  if (body.volPerTick !== undefined) { const n = Number(body.volPerTick); if (!Number.isFinite(n) || n < 0) return NextResponse.json({ error: "volPerTick invalid" }, { status: 400 }); patch.volPerTick = n; }
  updateWalk(patch);
  return NextResponse.json({ ok: true });
}
