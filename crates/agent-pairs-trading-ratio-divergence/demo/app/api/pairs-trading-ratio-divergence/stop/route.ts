import { NextResponse } from "next/server";
import { stopPairsTrading } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  stopPairsTrading();
  return NextResponse.json({ ok: true });
}
