import { NextResponse } from "next/server";
import { stopTargetAllocation } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  stopTargetAllocation();
  return NextResponse.json({ ok: true });
}
