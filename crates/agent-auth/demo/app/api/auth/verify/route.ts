import { NextResponse } from "next/server";
import { verifyCurrent, verifyAny } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(req: Request): Promise<Response> {
  let body: Record<string, unknown> = {};
  try { body = (await req.json()) as Record<string, unknown>; } catch { /* empty ok */ }
  const jwt = typeof body.jwt === "string" ? body.jwt.trim() : "";
  if (jwt.length > 0) {
    const res = verifyAny(jwt);
    return NextResponse.json(res);
  }
  const res = verifyCurrent();
  if (!res) return NextResponse.json({ error: "no current token — start auth or pass a jwt" }, { status: 400 });
  return NextResponse.json(res);
}
