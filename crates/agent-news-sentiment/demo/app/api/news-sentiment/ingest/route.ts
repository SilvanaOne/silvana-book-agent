import { NextResponse } from "next/server";
import { manualIngest } from "@/lib/store";
export const runtime = "nodejs";
export const dynamic = "force-dynamic";
export async function POST(req: Request): Promise<Response> {
  const body = (await req.json()) as Record<string, unknown>;
  const text = typeof body.text === "string" ? body.text.trim() : "";
  if (text.length === 0) return NextResponse.json({ error: "text required" }, { status: 400 });
  const s = manualIngest(text);
  return NextResponse.json({ ok: !!s });
}
