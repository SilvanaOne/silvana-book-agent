import { NextResponse } from "next/server";
import { stopMarketAbuse } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  stopMarketAbuse();
  return NextResponse.json({ ok: true });
}
