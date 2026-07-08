import { NextResponse } from "next/server";
import { decodeCurrent, decodeAny } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(req: Request): Promise<Response> {
  let body: Record<string, unknown> = {};
  try { body = (await req.json()) as Record<string, unknown>; } catch { /* empty ok */ }
  const jwt = typeof body.jwt === "string" ? body.jwt.trim() : "";
  try {
    if (jwt.length > 0) {
      const parsed = decodeAny(jwt);
      return NextResponse.json(parsed);
    }
    const parsed = decodeCurrent();
    if (!parsed) return NextResponse.json({ error: "no current token — start auth or pass a jwt" }, { status: 400 });
    return NextResponse.json(parsed);
  } catch (e) {
    return NextResponse.json({ error: (e as Error).message }, { status: 400 });
  }
}
