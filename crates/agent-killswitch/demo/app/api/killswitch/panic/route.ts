import { NextResponse } from "next/server";
import { panicKillswitch } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  panicKillswitch();
  return NextResponse.json({ ok: true });
}
