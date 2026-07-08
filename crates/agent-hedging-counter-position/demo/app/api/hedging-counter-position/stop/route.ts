import { NextResponse } from "next/server";
import { stopHedging } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  stopHedging();
  return NextResponse.json({ ok: true });
}
