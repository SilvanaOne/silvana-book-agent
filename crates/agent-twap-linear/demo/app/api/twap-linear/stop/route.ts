import { NextResponse } from "next/server";
import { stopTwap } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  stopTwap();
  return NextResponse.json({ ok: true });
}
