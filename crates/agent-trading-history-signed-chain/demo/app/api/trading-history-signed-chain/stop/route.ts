import { NextResponse } from "next/server";
import { stopTradingHistory } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  stopTradingHistory();
  return NextResponse.json({ ok: true });
}
