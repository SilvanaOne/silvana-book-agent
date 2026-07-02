import { NextResponse } from "next/server";
import { stopStateMonitor } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  stopStateMonitor();
  return NextResponse.json({ ok: true });
}
