import { NextResponse } from "next/server";
import { stopKillswitch } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  stopKillswitch();
  return NextResponse.json({ ok: true });
}
