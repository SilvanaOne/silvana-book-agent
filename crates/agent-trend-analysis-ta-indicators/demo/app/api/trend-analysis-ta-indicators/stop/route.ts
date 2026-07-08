import { NextResponse } from "next/server";
import { stopTrendAnalysis } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  stopTrendAnalysis();
  return NextResponse.json({ ok: true });
}
