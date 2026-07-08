import { NextResponse } from "next/server";
import { stopCircuitBreaker } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  stopCircuitBreaker();
  return NextResponse.json({ ok: true });
}
