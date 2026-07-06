import { NextResponse } from "next/server";
import { tamperAt } from "@/lib/store";
export const runtime = "nodejs";
export const dynamic = "force-dynamic";
export async function POST(req: Request): Promise<Response> {
  const body = (await req.json()) as Record<string, unknown>;
  const seq = Number(body.seq);
  if (!Number.isInteger(seq) || seq <= 0) return NextResponse.json({ error: "seq must be a positive integer" }, { status: 400 });
  const state = tamperAt(seq);
  return NextResponse.json({ ok: !!state });
}
