import { NextResponse } from "next/server";
import { stopWitnesses } from "@/lib/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(): Promise<Response> {
  stopWitnesses();
  return NextResponse.json({ ok: true });
}
