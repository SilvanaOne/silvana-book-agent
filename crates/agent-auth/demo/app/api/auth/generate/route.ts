import { NextResponse } from "next/server";
import { generateNow } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(req: Request): Promise<Response> {
  let body: Record<string, unknown> = {};
  try { body = (await req.json()) as Record<string, unknown>; } catch { /* empty body ok */ }
  const override: { role?: string; ttlSecs?: number } = {};
  if (typeof body.role === "string" && body.role.trim().length > 0) override.role = body.role.trim();
  if (body.ttlSecs !== undefined) {
    const n = Number(body.ttlSecs);
    if (!Number.isFinite(n) || n <= 0) {
      return NextResponse.json({ error: "ttlSecs must be a positive number" }, { status: 400 });
    }
    override.ttlSecs = n;
  }
  const token = generateNow(override);
  if (!token) return NextResponse.json({ error: "auth not started" }, { status: 400 });
  return NextResponse.json({ token });
}
