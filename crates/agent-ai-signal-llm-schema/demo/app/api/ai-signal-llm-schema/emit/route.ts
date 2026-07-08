import { NextResponse } from "next/server";
import { manualEmit } from "@/lib/store";
export const runtime = "nodejs";
export const dynamic = "force-dynamic";
export async function POST(req: Request): Promise<Response> {
  const body = (await req.json()) as Record<string, unknown>;
  const prompt = typeof body.prompt === "string" ? body.prompt.trim() : "";
  if (prompt.length === 0) return NextResponse.json({ error: "prompt required" }, { status: 400 });
  const s = manualEmit(prompt);
  return NextResponse.json({ ok: !!s });
}
