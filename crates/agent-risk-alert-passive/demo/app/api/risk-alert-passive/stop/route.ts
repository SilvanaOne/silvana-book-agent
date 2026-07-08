import { NextResponse } from "next/server";
import { stopRiskAlert } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  stopRiskAlert();
  return NextResponse.json({ ok: true });
}
