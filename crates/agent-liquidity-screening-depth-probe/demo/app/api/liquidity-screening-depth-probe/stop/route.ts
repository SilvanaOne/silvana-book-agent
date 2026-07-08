import { NextResponse } from "next/server";
import { stopLiquidityScreening } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  stopLiquidityScreening();
  return NextResponse.json({ ok: true });
}
