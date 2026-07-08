import { NextResponse } from "next/server";
import { stopBlockedParty } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  stopBlockedParty();
  return NextResponse.json({ ok: true });
}
