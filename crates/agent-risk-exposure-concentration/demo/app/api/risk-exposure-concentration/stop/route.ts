import { NextResponse } from "next/server";
import { stopRiskExposure } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  stopRiskExposure();
  return NextResponse.json({ ok: true });
}
