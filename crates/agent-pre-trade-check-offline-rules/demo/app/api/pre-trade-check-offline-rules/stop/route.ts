import { NextResponse } from "next/server";
import { stopPreTradeCheck } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  stopPreTradeCheck();
  return NextResponse.json({ ok: true });
}
