import { NextResponse } from "next/server";
import { tamperAt } from "@/lib/store";
export const runtime = "nodejs";
export const dynamic = "force-dynamic";
export async function POST(req: Request): Promise<Response> {
  const body = (await req.json()) as Record<string, unknown>;
  const num = Number(body.num);
  if (!Number.isInteger(num) || num <= 0) return NextResponse.json({ error: "num must be a positive integer" }, { status: 400 });
  const s = tamperAt(num);
  return NextResponse.json({ ok: !!s });
}
