import { NextResponse } from "next/server";
import { compileSpec } from "@/lib/store";
export const runtime = "nodejs";
export const dynamic = "force-dynamic";
export async function POST(req: Request): Promise<Response> {
  const body = (await req.json()) as Record<string, unknown>;
  const spec = typeof body.spec === "string" ? body.spec.trim() : "";
  if (spec.length === 0) return NextResponse.json({ error: "spec required" }, { status: 400 });
  const { step } = compileSpec(spec);
  return NextResponse.json({ step });
}
