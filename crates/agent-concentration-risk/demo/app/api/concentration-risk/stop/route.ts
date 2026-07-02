import { NextResponse } from "next/server";
import { stopConcentrationRisk } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  stopConcentrationRisk();
  return NextResponse.json({ ok: true });
}
