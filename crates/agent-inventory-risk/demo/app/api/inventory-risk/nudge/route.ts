import { NextResponse } from "next/server";
import { nudgeBalance } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(req: Request): Promise<Response> {
  const body = (await req.json()) as Record<string, unknown>;
  const delta = Number(body.delta);
  if (!Number.isFinite(delta)) return NextResponse.json({ error: "delta must be finite" }, { status: 400 });
  nudgeBalance(delta);
  return NextResponse.json({ ok: true });
}
