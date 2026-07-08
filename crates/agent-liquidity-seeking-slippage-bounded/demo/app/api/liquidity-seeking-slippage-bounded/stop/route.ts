import { NextResponse } from "next/server";
import { stopLiquiditySeeking } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  stopLiquiditySeeking();
  return NextResponse.json({ ok: true });
}
