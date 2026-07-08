import { NextResponse } from "next/server";
import { stopFairValue } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  stopFairValue();
  return NextResponse.json({ ok: true });
}
