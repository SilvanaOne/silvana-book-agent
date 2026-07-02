import { NextResponse } from "next/server";
import { stopYieldRotation } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  stopYieldRotation();
  return NextResponse.json({ ok: true });
}
